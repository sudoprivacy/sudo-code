use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::{self, IsTerminal, Write};

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{
    Cmd, CompletionType, ConditionalEventHandler, Config, Context, EditMode, Editor, EventContext,
    EventHandler, Helper, KeyCode, KeyEvent, Modifiers, RepeatCount,
};

/// Accept the line only when it contains non-whitespace text.
/// When the line is empty, Enter is a no-op.
struct AcceptNonEmpty;

impl ConditionalEventHandler for AcceptNonEmpty {
    fn handle(
        &self,
        _evt: &rustyline::Event,
        _n: RepeatCount,
        _positive: bool,
        ctx: &EventContext<'_>,
    ) -> Option<Cmd> {
        if ctx.line().trim().is_empty() {
            Some(Cmd::Noop)
        } else {
            Some(Cmd::AcceptLine)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    /// User submitted text input, possibly with attached image data.
    Submit {
        text: String,
        /// If the clipboard contained a new image since the last submit,
        /// this holds `(base64_data, mime_type)`.
        clipboard_image: Option<(String, String)>,
    },
    Exit,
}

struct SlashCommandHelper {
    /// Each entry is (command, description). Description may be empty.
    completions: Vec<(String, String)>,
    current_line: RefCell<String>,
}

impl SlashCommandHelper {
    fn new(completions: Vec<(String, String)>) -> Self {
        Self {
            completions: normalize_completions(completions),
            current_line: RefCell::new(String::new()),
        }
    }

    fn reset_current_line(&self) {
        self.current_line.borrow_mut().clear();
    }

    fn current_line(&self) -> String {
        self.current_line.borrow().clone()
    }

    fn set_current_line(&self, line: &str) {
        let mut current = self.current_line.borrow_mut();
        current.clear();
        current.push_str(line);
    }

    fn set_completions(&mut self, completions: Vec<(String, String)>) {
        self.completions = normalize_completions(completions);
    }
}

impl Completer for SlashCommandHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        let Some(prefix) = slash_command_prefix(line, pos) else {
            return Ok((0, Vec::new()));
        };

        let matches = self
            .completions
            .iter()
            .filter(|(cmd, _)| cmd.starts_with(prefix))
            .map(|(cmd, desc)| Pair {
                display: if desc.is_empty() {
                    cmd.clone()
                } else {
                    format!("{cmd:<24} — {desc}")
                },
                replacement: cmd.clone(),
            })
            .collect();

        Ok((0, matches))
    }
}

impl Hinter for SlashCommandHelper {
    type Hint = String;
}

impl Highlighter for SlashCommandHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        self.set_current_line(line);
        Cow::Borrowed(line)
    }

    fn highlight_char(&self, line: &str, _pos: usize, _kind: CmdKind) -> bool {
        self.set_current_line(line);
        false
    }
}

impl Validator for SlashCommandHelper {}
impl Helper for SlashCommandHelper {}

pub struct LineEditor {
    prompt: String,
    editor: Editor<SlashCommandHelper, DefaultHistory>,
    /// Whether the previous read returned a Ctrl-C on an empty prompt.
    pending_exit: bool,
    /// Hash of the last clipboard image that was already sent, so we don't
    /// re-attach a stale clipboard image on every submit.
    last_sent_image_hash: Option<String>,
}

impl LineEditor {
    #[must_use]
    pub fn new(prompt: impl Into<String>, completions: Vec<(String, String)>) -> Self {
        let config = Config::builder()
            .completion_type(CompletionType::List)
            .edit_mode(EditMode::Emacs)
            .build();
        let mut editor = Editor::<SlashCommandHelper, DefaultHistory>::with_config(config)
            .expect("rustyline editor should initialize");
        editor.set_helper(Some(SlashCommandHelper::new(completions)));
        editor.bind_sequence(KeyEvent(KeyCode::Char('J'), Modifiers::CTRL), Cmd::Newline);
        editor.bind_sequence(KeyEvent(KeyCode::Enter, Modifiers::SHIFT), Cmd::Newline);
        editor.bind_sequence(
            KeyEvent(KeyCode::Enter, Modifiers::NONE),
            EventHandler::Conditional(Box::new(AcceptNonEmpty)),
        );

        Self {
            prompt: prompt.into(),
            editor,
            pending_exit: false,
            last_sent_image_hash: None,
        }
    }

    pub fn push_history(&mut self, entry: impl Into<String>) {
        let entry = entry.into();
        if entry.trim().is_empty() {
            return;
        }

        let _ = self.editor.add_history_entry(entry);
    }

    pub fn set_completions(&mut self, completions: Vec<(String, String)>) {
        if let Some(helper) = self.editor.helper_mut() {
            helper.set_completions(completions);
        }
    }

    /// Probe the system clipboard for an image. If one is found and its hash
    /// differs from the last image we sent, register it and return
    /// `(base64_data, mime_type)`. If it's the same image we already sent,
    /// return `None` to avoid re-attaching a stale screenshot.
    fn take_clipboard_image(&mut self) -> Option<(String, String)> {
        let mut clipboard = arboard::Clipboard::new().ok()?;
        let img_data = clipboard.get_image().ok()?;
        let registry = runtime::ImageRegistry::default_cache().ok()?;
        let rgba: Vec<u8> = img_data.bytes.to_vec();
        let registered = registry
            .register_rgba(
                u32::try_from(img_data.width).unwrap_or(0),
                u32::try_from(img_data.height).unwrap_or(0),
                &rgba,
            )
            .ok()?;

        // Deduplicate: skip if this is the same image we already sent.
        if self.last_sent_image_hash.as_deref() == Some(&registered.hash) {
            return None;
        }

        self.last_sent_image_hash = Some(registered.hash.clone());
        let (b64, mime) = registry.load(&registered.hash).ok()?;
        Some((b64, mime))
    }

    pub fn read_line(&mut self) -> io::Result<ReadOutcome> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return self.read_line_fallback();
        }

        if let Some(helper) = self.editor.helper_mut() {
            helper.reset_current_line();
        }

        loop {
            match self.editor.readline(&self.prompt) {
                Ok(line) => {
                    self.pending_exit = false;
                    let clipboard_image = self.take_clipboard_image();
                    return Ok(ReadOutcome::Submit {
                        text: line,
                        clipboard_image,
                    });
                }
                Err(ReadlineError::Interrupted) => {
                    let has_input = !self.current_line().is_empty();
                    self.finish_interrupted_read();

                    let mut stdout = io::stdout();
                    // Undo rustyline's newline: move cursor back to prompt line, clear it.
                    write!(stdout, "\x1b[1F\x1b[2K")?;

                    if has_input {
                        // Had text — clear it and restart the prompt.
                        self.pending_exit = false;
                    } else if self.pending_exit {
                        // Second Ctrl-C — clear remaining chrome and exit.
                        writeln!(stdout, "\x1b[J")?;
                        stdout.flush()?;
                        return Ok(ReadOutcome::Exit);
                    } else {
                        self.pending_exit = true;
                        // Show exit hint in the footer area (2 lines below prompt).
                        write!(
                            stdout,
                            "\x1b[2E\x1b[2K  \x1b[2mPress Ctrl-C again to exit\x1b[0m\x1b[2F"
                        )?;
                    }

                    stdout.flush()?;
                    // Loop re-enters readline on the correct prompt line.
                }
                Err(ReadlineError::Eof) => {
                    self.finish_interrupted_read();
                    let mut stdout = io::stdout();
                    writeln!(stdout, "\x1b[J")?;
                    stdout.flush()?;
                    return Ok(ReadOutcome::Exit);
                }
                Err(error) => return Err(io::Error::other(error)),
            }
        }
    }

    fn current_line(&self) -> String {
        self.editor
            .helper()
            .map_or_else(String::new, SlashCommandHelper::current_line)
    }

    fn finish_interrupted_read(&mut self) {
        if let Some(helper) = self.editor.helper_mut() {
            helper.reset_current_line();
        }
    }

    fn read_line_fallback(&self) -> io::Result<ReadOutcome> {
        let mut stdout = io::stdout();
        write!(stdout, "{}", self.prompt)?;
        stdout.flush()?;

        let mut buffer = String::new();
        let bytes_read = io::stdin().read_line(&mut buffer)?;
        if bytes_read == 0 {
            return Ok(ReadOutcome::Exit);
        }

        while matches!(buffer.chars().last(), Some('\n' | '\r')) {
            buffer.pop();
        }
        Ok(ReadOutcome::Submit {
            text: buffer,
            clipboard_image: None,
        })
    }
}

fn slash_command_prefix(line: &str, pos: usize) -> Option<&str> {
    if pos != line.len() {
        return None;
    }

    let prefix = &line[..pos];
    if !prefix.starts_with('/') {
        return None;
    }

    Some(prefix)
}

fn normalize_completions(completions: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut seen = BTreeSet::new();
    completions
        .into_iter()
        .filter(|(cmd, _)| cmd.starts_with('/'))
        .filter(|(cmd, _)| seen.insert(cmd.clone()))
        .collect()
}
