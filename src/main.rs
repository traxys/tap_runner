use std::{
    process::Command,
    time::{Duration, Instant},
};

use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use itertools::Itertools;
use tap_parser::{DirectiveKind, TapParser, TapStatement, TapTest};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout},
    style::Color,
    text::Spans,
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
    reason: Option<String>,
}

#[derive(Debug)]
struct Test {
    result: bool,
    number: usize,
    desc: Option<String>,
    directive: Option<Directive>,

    parents: Vec<usize>,
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
    skipped: Vec<(String, Option<String>, Option<String>)>,
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
            skipped: Vec::new(),
        };

        if let Err(e) = this.run_tests() {
            this.err = Some(ErrorTracker::new(e));
        };

        this
    }

    fn run_tests(&mut self) -> anyhow::Result<()> {
        self.could_run = false;
        self.statuses.clear();
        self.skipped.clear();

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

        fn handle_body(
            body: Vec<TapStatement>,
            parents: Vec<usize>,
        ) -> impl Iterator<Item = Test> + '_ {
            body.into_iter()
                .enumerate()
                .flat_map(move |(i, st)| handle_statement(st, i, parents.clone()))
        }

        fn handle_statement(
            statement: TapStatement,
            number: usize,
            parents: Vec<usize>,
        ) -> impl Iterator<Item = Test> + '_ {
            fn handle_test_point(test: TapTest, parents: Vec<usize>, number: usize) -> Test {
                Test {
                    result: test.result,
                    number: test.number.unwrap_or(number),
                    desc: test.desc.map(ToString::to_string),
                    directive: test.directive.as_ref().map(|d| Directive {
                        key: match &d.kind {
                            DirectiveKind::Skip => DirectiveKind::Skip,
                            DirectiveKind::Todo => DirectiveKind::Todo,
                        },
                        reason: d.reason.map(ToString::to_string),
                    }),
                    parents: parents.to_vec(),
                }
            }

            match statement {
                TapStatement::Subtest(s) => {
                    let mut child_lineage = parents.to_vec();
                    child_lineage.push(number);
                    let b: Box<dyn Iterator<Item = _>> =
                        Box::new(handle_body(s.statements, child_lineage));
                    Either3::One(b.chain(std::iter::once(handle_test_point(
                        s.ending, parents, number,
                    ))))
                }
                TapStatement::TestPoint(t) => {
                    Either3::Two(std::iter::once(handle_test_point(t, parents, number)))
                }
                _ => Either3::Three(std::iter::empty()),
            }
        }

        self.statuses.clear();
        self.skipped.clear();
        for test in handle_body(document, Vec::new()) {
            let number = test
                .parents
                .iter()
                .chain(std::iter::once(&test.number))
                .join(".");
            if !test.result {
                self.statuses.push(TestResult::Fail);
            } else {
                match test.directive {
                    Some(d) if d.key == tap_parser::DirectiveKind::Skip => {
                        self.skipped.push((number, test.desc, d.reason));
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

        let skipped_constraint = if self.skipped.is_empty() {
            Constraint::Max(0)
        } else if self.skipped.len() <= 10 {
            Constraint::Max((2 + self.skipped.len()) as u16)
        } else {
            Constraint::Max(12)
        };

        let error_constraint = if self.err.is_none() {
            Constraint::Max(0)
        } else if self.could_run {
            Constraint::Max(4)
        } else {
            Constraint::Min(0)
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([error_constraint, Constraint::Max(5), skipped_constraint])
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

        if !self.skipped.is_empty() {
            let p = Paragraph::new(
                self.skipped
                    .iter()
                    .map(|(parents, desc, reason)| {
                        Spans::from(
                            parents.clone()
                                + &match desc {
                                    None => "".into(),
                                    Some(d) => format!(" - {d}"),
                                }
                                + &match reason {
                                    None => "".into(),
                                    Some(r) => format!(" ({r})"),
                                },
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .block(Block::default().title("Skipped").borders(Borders::ALL));
            f.render_widget(p, chunks[2])
        }
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
