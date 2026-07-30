use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;
use std::io::{self, stdout, Stdout};

pub type Terminal = ratatui::Terminal<CrosstermBackend<Stdout>>;

pub struct TerminalGuard {
    terminal: Option<Terminal>,
    raw_entered: bool,
}

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut out = stdout();
        if let Err(e) = crossterm::execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        ) {
            // staged cleanup on partial init: restore every state already enabled
            let _ = crossterm::execute!(
                stdout(),
                DisableMouseCapture,
                LeaveAlternateScreen,
                DisableBracketedPaste
            );
            let _ = disable_raw_mode();
            return Err(e);
        }
        let backend = CrosstermBackend::new(out);
        let terminal = match ratatui::Terminal::new(backend) {
            Ok(t) => t,
            Err(e) => {
                let _ = crossterm::execute!(
                    stdout(),
                    DisableMouseCapture,
                    LeaveAlternateScreen,
                    DisableBracketedPaste
                );
                let _ = disable_raw_mode();
                return Err(e);
            }
        };
        Ok(TerminalGuard {
            terminal: Some(terminal),
            raw_entered: true,
        })
    }

    /// Safe mutable borrow of the terminal for drawing / event loop use.
    /// Does not transfer ownership; restoration ownership stays with the guard and remains consuming + exactly-once via restore(self).
    pub fn terminal_mut(&mut self) -> Option<&mut Terminal> {
        self.terminal.as_mut()
    }

    /// Consuming restore: exactly once, by ownership. Always runs the disable sequence
    /// (mouse, alternate, bracketed-paste, raw). Terminal is only ever borrowed mut (via terminal_mut) for draws; never taken out.
    /// No Drop performs restoration.
    pub fn restore(mut self) -> io::Result<()> {
        let mut out = std::io::stdout();
        let _ = crossterm::execute!(
            out,
            DisableMouseCapture,
            LeaveAlternateScreen,
            DisableBracketedPaste
        );
        if self.raw_entered {
            let _ = disable_raw_mode();
            self.raw_entered = false;
        }
        if let Some(mut t) = self.terminal.take() {
            let _ = t.flush();
        }
        Ok(())
    }
}
