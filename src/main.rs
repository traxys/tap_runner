use std::{
    process::Command,
    time::{Duration, Instant},
};

use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame, Terminal,
};

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
struct App {
    test_command: String,
    test_args: Vec<String>,

    err: Option<ErrorTracker>,
}

impl App {
    fn new(test: Vec<String>) -> Self {
        let mut test = test.into_iter();
        let test_command = test.next().unwrap();

        let mut this = Self {
            test_command,
            test_args: test.collect(),
            err: None,
        };

        if let Err(e) = this.run_tests() {
            this.err = Some(ErrorTracker::new(e));
        };

        this
    }

    fn run_tests(&mut self) -> anyhow::Result<()> {
        let mut command = Command::new(&self.test_command);
        command.args(&self.test_args);
        let _output = command.output()?;

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
        } else {
            Constraint::Min(0)
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([error_constraint])
            .split(inner);

        if let Some(e) = &self.err {
            let p = Paragraph::new(e.error.clone())
                .block(Block::default().title("Error").borders(Borders::ALL))
                .wrap(Wrap { trim: true });
            f.render_widget(p, chunks[0]);
        }
    }
}

#[derive(Parser, Debug)]
struct Args {
    #[arg(required = true)]
    run_command: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = App::new(args.run_command).run(&mut terminal, Duration::from_secs_f64(0.1));

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    res
}
