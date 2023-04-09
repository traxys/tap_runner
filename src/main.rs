use std::{
    env,
    path::Path,
    process::Command,
    str::FromStr,
    time::{Duration, Instant},
};

use ansi_to_tui::IntoText;
use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use itertools::Itertools;
use jaq_core::{Definitions, Filter};
use tap_parser::{DirectiveKind, TapParser, TapStatement, TapTest};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::Color,
    text::{Span, Spans, Text},
    widgets::{Block, BorderType, Borders, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};

use widgets::{ColoredList, StatefulList};
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
    yaml: String,
    location: Option<Location>,

    parents: Vec<usize>,
}

#[derive(Debug)]
struct Location {
    file: String,
    line: usize,
}

impl FromStr for Location {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((file, line)) = s.split_once(':') else {
            anyhow::bail!("Missing `:` in location")
        };

        Ok(Self {
            file: file.into(),
            line: line.parse()?,
        })
    }
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

    preview: bool,

    location_filter: Option<Filter>,

    err: Option<ErrorTracker>,

    statuses: Vec<TestResult>,
    skipped: Vec<(String, Option<String>, Option<String>)>,
    failure: StatefulList<(String, Option<String>, String, Option<Location>)>,
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
    fn new(
        test: Vec<String>,
        build: Option<Vec<String>>,
        location_filter: Option<String>,
        preview: bool,
    ) -> anyhow::Result<Self> {
        if preview {
            match which::which("bat") {
                Ok(_) => (),
                Err(which::Error::CannotFindBinaryPath) => {
                    anyhow::bail!("Can't find executable `bat`, could not enable --preview");
                }
                Err(e) => {
                    anyhow::bail!("Error in checking for conditions of preview: {e}")
                }
            }
        };

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
            preview,
            statuses: Vec::new(),
            skipped: Vec::new(),
            failure: StatefulList::empty(),
            location_filter: location_filter
                .map(|f| -> anyhow::Result<_> {
                    let defs = Definitions::core();

                    let (f, errs) = jaq_core::parse::parse(&f, jaq_core::parse::main());
                    let f = match f {
                        None => {
                            anyhow::bail!("Errors parsing the filter: {}", errs.iter().join("\n"))
                        }
                        Some(f) => f,
                    };
                    let mut errs = Vec::new();
                    let f = defs.finish(f, Vec::new(), &mut errs);
                    if !errs.is_empty() {
                        anyhow::bail!("Errors finishing the filter: {}", errs.iter().join("\n"))
                    }

                    Ok(f)
                })
                .transpose()?,
        };

        if let Err(e) = this.run_tests() {
            this.err = Some(ErrorTracker::new(e));
        };

        Ok(this)
    }

    fn run_tests(&mut self) -> anyhow::Result<()> {
        self.could_run = false;
        self.statuses.clear();
        self.skipped.clear();
        self.failure = StatefulList::empty();

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

        fn handle_body<'a, 'f: 'a>(
            body: Vec<TapStatement<'a>>,
            parents: Vec<usize>,
            filter: &'f Option<Filter>,
        ) -> impl Iterator<Item = (Test, Option<ErrorTracker>)> + 'a {
            body.into_iter()
                .enumerate()
                .flat_map(move |(i, st)| handle_statement(st, i, parents.clone(), filter))
        }

        fn handle_statement<'a, 'f: 'a>(
            statement: TapStatement<'a>,
            number: usize,
            parents: Vec<usize>,
            filter: &'f Option<Filter>,
        ) -> impl Iterator<Item = (Test, Option<ErrorTracker>)> + 'a {
            fn handle_test_point(
                test: TapTest,
                parents: Vec<usize>,
                number: usize,
                filter: &Option<Filter>,
            ) -> (Test, Option<ErrorTracker>) {
                let mut err = None;
                let yaml = test.yaml.join("\n");
                let location = match filter {
                    Some(f) if !yaml.is_empty() => {
                        match serde_yaml::from_str::<serde_yaml::Value>(&yaml) {
                            Ok(v) => {
                                let json = serde_json::to_value(&v)
                                    .expect("Could not parse back YAML into JSON");
                                let inputs = jaq_core::RcIter::new(core::iter::empty());
                                let mut out = f.run(
                                    jaq_core::Ctx::new([], &inputs),
                                    jaq_core::Val::from(json),
                                );
                                match out.next().map(|v| v.map(|r| r.to_str().map(|s| s.parse()))) {
                                    None => None,
                                    Some(Err(e)) | Some(Ok(Err(e))) => {
                                        err = Some(ErrorTracker::new(e));
                                        None
                                    }
                                    Some(Ok(Ok(Err(e)))) => {
                                        err = Some(ErrorTracker::new(e));
                                        None
                                    }
                                    Some(Ok(Ok(Ok(v)))) => Some(v),
                                }
                            }
                            Err(e) => {
                                err = Some(ErrorTracker::new(e));
                                None
                            }
                        }
                    }
                    _ => None,
                };
                (
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
                        yaml,
                        location,
                        parents: parents.to_vec(),
                    },
                    err,
                )
            }

            match statement {
                TapStatement::Subtest(s) => {
                    let mut child_lineage = parents.to_vec();
                    child_lineage.push(number);
                    let b: Box<dyn Iterator<Item = _>> =
                        Box::new(handle_body(s.statements, child_lineage, filter));
                    Either3::One(b.chain(std::iter::once(handle_test_point(
                        s.ending, parents, number, filter,
                    ))))
                }
                TapStatement::TestPoint(t) => Either3::Two(std::iter::once(handle_test_point(
                    t, parents, number, filter,
                ))),
                _ => Either3::Three(std::iter::empty()),
            }
        }

        self.statuses.clear();
        self.skipped.clear();
        let mut failure = Vec::new();
        for (test, err) in handle_body(document, Vec::new(), &self.location_filter) {
            let number = test
                .parents
                .iter()
                .chain(std::iter::once(&test.number))
                .join(".");
            if !test.result {
                failure.push((number, test.desc, test.yaml, test.location));
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
            self.err = self.err.take().or(err);
        }
        self.failure = StatefulList::with_items(failure);

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
                        KeyCode::Up => self.failure.previous(),
                        KeyCode::Down => self.failure.next(),
                        KeyCode::Esc => self.failure.unselect(),
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

        let body_constraint = if self.could_run {
            Constraint::Min(0)
        } else {
            Constraint::Max(0)
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                error_constraint,
                Constraint::Max(5),
                skipped_constraint,
                body_constraint,
            ])
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

        let mut failure_location = chunks[3];
        if self.preview {
            if let Some((_, _, _, Some(location))) = self.failure.selected() {
                let preview_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(chunks[3]);

                match generate_failure_preview(location, preview_chunks[1]) {
                    Ok(p) => {
                        f.render_widget(
                            Paragraph::new(p).block(Block::default().borders(Borders::all())),
                            preview_chunks[1],
                        );
                        failure_location = preview_chunks[0];
                    }
                    Err(e) => {
                        self.err = Some(ErrorTracker::new(e));
                    }
                };
            }
        }

        self.failure
            .render(f, failure_location, |(num, desc, yaml, location)| {
                let mut lines = Vec::new();
                lines.push(
                    Span::raw(
                        num.clone()
                            + &match desc {
                                None => "".into(),
                                Some(d) => format!(" - {d}"),
                            },
                    )
                    .into(),
                );
                lines.push("----------".into());
                if let Some(location) = location {
                    lines.push(
                        format!("Failure in '{}' at line {}", location.file, location.line).into(),
                    );
                };
                lines.extend(
                    yaml.split('\n')
                        .filter(|s| !s.is_empty())
                        .map(|t| Spans::from(t.to_owned())),
                );
                lines.push("----------".into());
                ListItem::new(lines)
            });
    }
}

fn generate_failure_preview(location: &Location, area: Rect) -> anyhow::Result<Text> {
    if !Path::new(&location.file).exists() {
        anyhow::bail!("File {} does not exist", location.file)
    }

    let shell = env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
    let mut preview = Command::new(shell)
        .arg("-c")
        .arg(format!(
            "bat --force-colorization --terminal-width {} {} --highlight-line {}",
            area.width - 2,
            location.file,
            location.line
        ))
        .output()?
        .stdout
        .into_text()?;

    let height = area.height - 2;

    if location.line > height as usize {
        let out = location.line - height as usize;
        let discard_to_center = out + (height as usize) / 2;
        let would_remain = preview.lines.len() - discard_to_center;
        let discard = if would_remain > height as usize {
            discard_to_center
        } else {
            discard_to_center - (height as usize - would_remain)
        };
        preview.lines.drain(0..discard);
    }

    Ok(preview)
}

#[derive(Parser, Debug)]
struct Args {
    #[arg(required = true)]
    run_command: Vec<String>,
    #[arg(long, short, value_delimiter = ',')]
    build_command: Option<Vec<String>>,
    #[arg(long, short)]
    location_filter: Option<String>,
    #[arg(long, short, requires = "location_filter")]
    preview: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = App::new(
        args.run_command,
        args.build_command,
        args.location_filter,
        args.preview,
    )?
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
