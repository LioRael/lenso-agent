//! RAII ownership of raw mode, alternate screen, mouse capture, and cursor state.

use super::{
    CrosstermBackend, DisableMouseCapture, EnableMouseCapture, EnterAlternateScreen,
    LeaveAlternateScreen, Terminal, disable_raw_mode, enable_raw_mode, execute, io,
};

pub(super) struct TerminalSession {
    pub(super) terminal: Terminal<CrosstermBackend<io::Stdout>>,
    restored: bool,
}

impl TerminalSession {
    pub(super) fn start() -> Result<Self, String> {
        enable_raw_mode()
            .map_err(|error| format!("failed to enable terminal raw mode: {error}"))?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(format!("failed to enter alternate screen: {error}"));
        }
        if let Err(error) = execute!(stdout, EnableMouseCapture) {
            let _ = execute!(stdout, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(format!("failed to enable terminal mouse capture: {error}"));
        }
        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = disable_raw_mode();
                let mut stdout = io::stdout();
                let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
                return Err(format!("failed to initialize terminal: {error}"));
            }
        };
        Ok(Self {
            terminal,
            restored: false,
        })
    }

    pub(super) fn restore(&mut self) -> Result<(), String> {
        if self.restored {
            return Ok(());
        }
        let raw_mode = disable_raw_mode();
        let mouse = execute!(self.terminal.backend_mut(), DisableMouseCapture);
        let alternate_screen = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let cursor = self.terminal.show_cursor();
        self.restored = true;

        raw_mode.map_err(|error| format!("failed to disable terminal raw mode: {error}"))?;
        mouse.map_err(|error| format!("failed to disable terminal mouse capture: {error}"))?;
        alternate_screen.map_err(|error| format!("failed to leave alternate screen: {error}"))?;
        cursor.map_err(|error| format!("failed to restore terminal cursor: {error}"))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if !self.restored {
            let _ = disable_raw_mode();
            let _ = execute!(self.terminal.backend_mut(), DisableMouseCapture);
            let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
            let _ = self.terminal.show_cursor();
        }
    }
}
