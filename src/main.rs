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
    layout::Alignment,
    widgets::{Block, BorderType, Borders},
    Frame, Terminal,
};

struct App {
    test_command: String,
    test_args: Vec<String>,
}

impl App {
    fn new(test: Vec<String>) -> Self {
        let mut test = test.into_iter();
        let test_command = test.next().unwrap();

        let mut this = Self {
            test_command,
            test_args: test.collect(),
        };

        let _ = this.run_tests();

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
                            self.run_tests()?;
                        }
                        _ => (),
                    }
                }
            }

            if last_tick.elapsed() >= tick_rate {
                last_tick = Instant::now();
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
        let _inner = outer.inner(size);
        f.render_widget(outer, size);
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
