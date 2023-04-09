use std::{
    process::Command,
    time::{Duration, Instant},
};

use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use tap_parser::{DirectiveKind, TapParser, TapStatement, TapTest};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout},
    style::Color,
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame, Terminal,
};

use widgets::ColoredList;
mod widgets;

pub struct ErrorTracker {
    error: String,
    created_at: Instant,
}

impl ErrorTracker {
    pub fn new<E>(e: E) -> Self
    where
        E: ToString,
    {
        Self {
            error: e.to_string(),
            created_at: Instant::now(),
        }
    }
}

#[derive(Debug)]
struct Directive {
    key: tap_parser::DirectiveKind,
}

#[derive(Debug)]
struct Test {
    result: bool,
    directive: Option<Directive>,
}

enum TestResult {
    Skip,
    Success,
    Fail,
}

struct App {
    test_command: String,
    test_args: Vec<String>,
    build_command: Option<String>,
    build_args: Vec<String>,

    err: Option<ErrorTracker>,

    statuses: Vec<TestResult>,
    could_run: bool,
}

enum Either3<T, U, V> {
    One(T),
    Two(U),
    Three(V),
}

impl<T, U, V, X> Iterator for Either3<T, U, V>
where
    T: Iterator<Item = X>,
    U: Iterator<Item = X>,
    V: Iterator<Item = X>,
{
    type Item = X;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Either3::One(v) => v.next(),
            Either3::Two(v) => v.next(),
            Either3::Three(v) => v.next(),
        }
    }
}

impl App {
    fn new(test: Vec<String>, build: Option<Vec<String>>) -> Self {
        let mut test = test.into_iter();
        let test_command = test.next().unwrap();
        let (build_command, build_args) = match build {
            None => (None, Vec::new()),
            Some(b) => {
                let mut build = b.into_iter();
                (build.next(), build.collect())
            }
        };

        let mut this = Self {
            test_command,
            test_args: test.collect(),
            build_command,
            build_args,
            err: None,
            could_run: true,
            statuses: Vec::new(),
        };

        if let Err(e) = this.run_tests() {
            this.err = Some(ErrorTracker::new(e));
        };

        this
    }

    fn run_tests(&mut self) -> anyhow::Result<()> {
        self.could_run = false;
        self.statuses.clear();

        if let Some(build) = &self.build_command {
            let result = duct::cmd(build, &self.build_args)
                .stderr_to_stdout()
                .stdout_capture()
                .unchecked()
                .run()?;
            if !result.status.success() {
                anyhow::bail!(
                    "Build command failed: {}",
                    String::from_utf8_lossy(&result.stdout)
                )
            }
        }
        self.could_run = true;

        let mut command = Command::new(&self.test_command);
        command.args(&self.test_args);
        let output = command.output()?;

        let tap = String::from_utf8(output.stdout)?;
        let mut parser = TapParser::new();
        let document = parser.parse(&tap)?;

        fn handle_body(body: Vec<TapStatement>) -> impl Iterator<Item = Test> + '_ {
            body.into_iter().flat_map(handle_statement)
        }

        fn handle_statement(statement: TapStatement) -> impl Iterator<Item = Test> + '_ {
            fn handle_test_point(test: TapTest) -> Test {
                Test {
                    result: test.result,
                    directive: test.directive.as_ref().map(|d| Directive {
                        key: match &d.kind {
                            DirectiveKind::Skip => DirectiveKind::Skip,
                            DirectiveKind::Todo => DirectiveKind::Todo,
                        },
                    }),
                }
            }

            match statement {
                TapStatement::Subtest(s) => {
                    let b: Box<dyn Iterator<Item = _>> = Box::new(handle_body(s.statements));
                    Either3::One(b.chain(std::iter::once(handle_test_point(s.ending))))
                }
                TapStatement::TestPoint(t) => Either3::Two(std::iter::once(handle_test_point(t))),
                _ => Either3::Three(std::iter::empty()),
            }
        }

        self.statuses.clear();
        for test in handle_body(document) {
            if !test.result {
                self.statuses.push(TestResult::Fail);
            } else {
                match test.directive {
                    Some(d) if d.key == tap_parser::DirectiveKind::Skip => {
                        self.statuses.push(TestResult::Skip);
                    }
                    _ => self.statuses.push(TestResult::Success),
                };
            }
        }

        Ok(())
    }

    fn run<B: Backend>(
        mut self,
        terminal: &mut Terminal<B>,
        tick_rate: Duration,
    ) -> anyhow::Result<()> {
        let mut last_tick = Instant::now();
        loop {
            terminal.draw(|f| self.draw(f))?;

            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or(Duration::from_secs(0));
            if crossterm::event::poll(timeout)? {
                if let Event::Key(key) = crossterm::event::read()? {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('r') => {
                            if let Err(e) = self.run_tests() {
                                self.err = Some(ErrorTracker::new(e));
                            }
                        }
                        _ => (),
                    }
                }
            }

            if last_tick.elapsed() >= tick_rate {
                last_tick = Instant::now();

                if let Some(true) = self
                    .err
                    .as_ref()
                    .map(|err| err.created_at.elapsed() > Duration::from_secs(8))
                {
                    self.err = None;
                }
            }
        }
    }

    fn draw<B: Backend>(&mut self, f: &mut Frame<B>) {
        let size = f.size();
        let outer = Block::default()
            .borders(Borders::ALL)
            .title("TAP Runner")
            .title_alignment(Alignment::Center)
            .border_type(BorderType::Rounded);
        let inner = outer.inner(size);
        f.render_widget(outer, size);

        let error_constraint = if self.err.is_none() {
            Constraint::Max(0)
        } else if self.could_run {
            Constraint::Max(4)
        } else {
            Constraint::Min(0)
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([error_constraint, Constraint::Max(5)])
            .split(inner);

        if let Some(e) = &self.err {
            let p = Paragraph::new(e.error.clone())
                .block(Block::default().title("Error").borders(Borders::ALL))
                .wrap(Wrap { trim: true });
            f.render_widget(p, chunks[0]);
        }

        let status = ColoredList::new(
            self.statuses
                .iter()
                .map(|s| match s {
                    TestResult::Skip => Color::Yellow,
                    TestResult::Success => Color::Blue,
                    TestResult::Fail => Color::Rgb(255, 0, 0),
                })
                .collect(),
        )
        .block(Block::default().title("Status").borders(Borders::ALL));
        f.render_widget(status, chunks[1]);
    }
}

#[derive(Parser, Debug)]
struct Args {
    #[arg(required = true)]
    run_command: Vec<String>,
    #[arg(long, short, value_delimiter = ',')]
    build_command: Option<Vec<String>>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = App::new(args.run_command, args.build_command)
        .run(&mut terminal, Duration::from_secs_f64(0.1));

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    res
}
