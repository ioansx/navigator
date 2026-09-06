use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Constraint, Layout, Margin, Offset, Rect},
    prelude::Buffer,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, StatefulWidget, Widget},
};
use ratatui_image::{StatefulImage, picker::Picker, protocol::StatefulProtocol};

use crate::{
    error::{Errx, Resultx},
    globals::{
        ACCENT, CURSOR_BAR, DIM, MARK, MARK_DOT, NF_OCT_FILE_DIRECTORY_FILL, SCROLL_JUMP,
        SCROLL_OFF, SEARCH, SEARCH_TEXT, file_color, level_color,
    },
    io::{
        FileKind, clipboard,
        dir::{self, DirEntry},
        file, fs_ops, nvim, raster, zoxide,
    },
    log_store::{LOG_STORE, LogEntry},
    marks::Marks,
    memory::Memory,
    plan::{Op, Plan, Staged, validate_name},
};

/// How many staged operations the plan panel shows before it stops growing.
const PLAN_PANEL_ROWS: usize = 6;

/// What a keypress leaves the session doing.
///
/// Quitting comes in two kinds because only one of them is allowed to move the
/// shell: `q` leaves it where it was, `Q` takes it along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Quit,
    /// Quit, and hand back the directory the session ended in.
    QuitHere,
}

/// What the keyboard currently means.
pub enum Mode {
    Normal,
    /// Looking at the staged plan, deciding whether to run it.
    Review,
    /// Typing a name.
    Prompt(Prompt),
    /// Reading the key map.
    Help,
}

struct HelpSection {
    title: &'static str,
    keys: &'static [(&'static str, &'static str)],
}

/// The key map, kept directly above the handler it describes so the two are read
/// and edited together.
const HELP: &[HelpSection] = &[
    HelpSection {
        title: "moving around",
        keys: &[
            ("j / k", "up and down"),
            ("ctrl-d/u", "jump by a screenful"),
            ("enter / l", "enter dir, or open in neovim"),
            ("- / h", "go up a level"),
            ("z", "jump to a zoxide directory"),
            ("/", "search the listing"),
            ("n / N", "next, previous match"),
            ("q", "quit"),
            ("Q", "quit, and take the shell here"),
        ],
    },
    HelpSection {
        title: "marking",
        keys: &[
            ("space", "mark or unmark"),
            ("esc", "clear search, then marks, then plan"),
        ],
    },
    HelpSection {
        title: "staging",
        keys: &[
            ("c", "copy marked here"),
            ("m", "move marked here"),
            ("d", "trash marked"),
            ("a", "new file"),
            ("A", "new directory"),
            ("r", "rename this entry"),
        ],
    },
    HelpSection {
        title: "the plan",
        keys: &[
            ("p", "review the plan"),
            ("enter", "apply it"),
            ("x", "drop this operation"),
            ("X", "drop everything blocked"),
            ("r", "rename its destination"),
            ("esc", "back, keeping the plan"),
        ],
    },
    HelpSection {
        title: "and also",
        keys: &[
            ("y", "contents to the clipboard"),
            ("L", "show the log"),
            ("?", "these keys"),
        ],
    },
];

pub struct Prompt {
    label: &'static str,
    input: String,
    action: PromptAction,
}

enum PromptAction {
    CreateFile,
    CreateDir,
    Rename(PathBuf),
    /// Change where staged operation `n` writes to.
    Retarget(usize),
    /// Go wherever zoxide ranks the typed words highest.
    Jump,
    /// Walk the listing as the query is typed, from the row it started on.
    Search {
        origin: usize,
    },
}

pub struct Navigator {
    current_dir: PathBuf,
    entries: Vec<DirEntry>,
    selected: usize,
    scroll_offset: usize,
    picker: Picker,
    cached_image: Option<(PathBuf, StatefulProtocol)>,
    log_panel_visible: bool,
    log_scroll_offset: usize,
    memory: Memory,
    marks: Marks,
    plan: Plan,
    mode: Mode,
    review_selected: usize,
    /// The last thing searched for, which `n` and `N` repeat.
    search: String,
}

impl Navigator {
    pub fn new(current_dir_path: &str, select: Option<&str>) -> Resultx<Self> {
        let current_dir = dir::resolve(Path::new(current_dir_path))?;
        let entries = dir::read_dir_with_dots(&current_dir)?;
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

        let selected = select
            .and_then(|name| index_of(&entries, name))
            .unwrap_or(0);

        Ok(Self {
            current_dir,
            entries,
            selected,
            scroll_offset: 0,
            picker,
            cached_image: None,
            log_panel_visible: false,
            log_scroll_offset: 0,
            memory: Memory::default(),
            marks: Marks::default(),
            plan: Plan::default(),
            mode: Mode::Normal,
            review_selected: 0,
            search: String::new(),
        })
    }

    /// The directory being viewed, which is where a session ends up.
    pub fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    /// Handles one keypress. Returns `true` when the navigator should quit.
    ///
    /// Nothing a key does is worth ending the session over: a directory that
    /// cannot be read, a neovim that is not installed, a listing that cannot be
    /// re-read — each is a line on the status bar, and the navigator stays where
    /// it is with its marks and its plan intact.
    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        let outcome = match self.mode {
            Mode::Prompt(_) => {
                self.handle_prompt_key(key);
                Ok(Outcome::Stay)
            }
            Mode::Review => self.handle_review_key(key),
            Mode::Normal => self.handle_normal_key(key),
            Mode::Help => {
                // Anything at all dismisses it: nobody should have to guess twice.
                self.mode = Mode::Normal;
                Ok(Outcome::Stay)
            }
        };

        outcome.unwrap_or_else(|e| {
            log::error!("{e}");
            Outcome::Stay
        })
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> Resultx<Outcome> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Char('d') if ctrl => self.move_down_by(SCROLL_JUMP),
            KeyCode::Char('u') if ctrl => self.move_up_by(SCROLL_JUMP),
            // Every other letter is a binding only when nothing but shift is held
            // with it. ctrl-c means "get me out of here" everywhere else, and must
            // not land on the copy that plain `c` is.
            KeyCode::Char(_) if held(key) => {}
            KeyCode::Char('q') => {
                log::info!("Navigator quit");
                return Ok(Outcome::Quit);
            }
            KeyCode::Char('Q') => {
                log::info!("Navigator quit in {}", self.current_dir.display());
                return Ok(Outcome::QuitHere);
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_down(),
            KeyCode::Char('k') | KeyCode::Up => self.move_up(),
            KeyCode::Enter | KeyCode::Char('l') => return self.enter_selected(),
            KeyCode::Char('-' | 'h') => self.go_to_parent_directory()?,
            KeyCode::Char('z') => self.begin_prompt("jump to", PromptAction::Jump),
            KeyCode::Char('/') => {
                self.begin_prompt(
                    "search",
                    PromptAction::Search {
                        origin: self.selected,
                    },
                );
            }
            KeyCode::Char('n') => {
                self.select_match(indices_from(self.entries.len(), self.selected + 1));
            }
            KeyCode::Char('N') => {
                self.select_match(indices_from(self.entries.len(), self.selected).rev());
            }
            KeyCode::Char('L') => self.toggle_log_panel(),

            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Esc => self.back_out(),
            KeyCode::Char('c') => self.stage_into_here(|from, to| Op::Copy { from, to }),
            KeyCode::Char('m') => self.stage_into_here(|from, to| Op::Move { from, to }),
            KeyCode::Char('d') => self.stage_trash(),
            KeyCode::Char('a') => self.begin_prompt("new file", PromptAction::CreateFile),
            KeyCode::Char('A') => self.begin_prompt("new directory", PromptAction::CreateDir),
            KeyCode::Char('r') => self.begin_rename(),
            KeyCode::Char('y') => self.yank_to_clipboard(),
            KeyCode::Char('p') => self.open_review(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
        Ok(Outcome::Stay)
    }

    fn handle_review_key(&mut self, key: KeyEvent) -> Resultx<Outcome> {
        match key.code {
            KeyCode::Char('q') => return Ok(Outcome::Quit),
            KeyCode::Char('Q') => return Ok(Outcome::QuitHere),
            KeyCode::Esc | KeyCode::Char('p') => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => {
                self.review_selected = (self.review_selected + 1).min(self.plan.len().max(1) - 1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.review_selected = self.review_selected.saturating_sub(1);
            }
            KeyCode::Char('x') => {
                self.plan.remove(self.review_selected);
                self.clamp_review();
            }
            KeyCode::Char('X') => {
                self.plan.drop_blocked(|op| fs_ops::problem(op).is_some());
                self.clamp_review();
            }
            KeyCode::Char('r') => self.begin_retarget(),
            KeyCode::Enter => self.apply_plan()?,
            _ => {}
        }
        Ok(Outcome::Stay)
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        let Mode::Prompt(prompt) = &mut self.mode else {
            return;
        };

        match key.code {
            // ctrl-something is a command someone expects, not a letter they want
            // in the name they are typing.
            KeyCode::Char(_) if held(key) => return,
            KeyCode::Char(c) => prompt.input.push(c),
            KeyCode::Backspace => {
                prompt.input.pop();
            }
            KeyCode::Esc => {
                self.abandon_prompt();
                return;
            }
            KeyCode::Enter => {
                self.confirm_prompt();
                return;
            }
            _ => return,
        }

        // What was typed changed. Only a search acts on that at once; every other
        // prompt waits for enter.
        self.follow_search();
    }

    /// Leaves the prompt with nothing done. A search also puts the cursor back
    /// where it started: typing one was a way of looking around, not of moving.
    fn abandon_prompt(&mut self) {
        let Mode::Prompt(prompt) = &self.mode else {
            return;
        };

        self.mode = match prompt.action {
            PromptAction::Search { origin } => {
                self.selected = origin;
                // An abandoned search is not one `n` should repeat.
                self.search.clear();
                Mode::Normal
            }
            // Back to the screen the prompt was opened from, which for this one is
            // the review: cancelling a rename is not a reason to leave it.
            PromptAction::Retarget(_) => Mode::Review,
            _ => Mode::Normal,
        };
    }

    /// Moves the cursor to what is being typed, so the search answers as it is
    /// written rather than when it is finished.
    fn follow_search(&mut self) {
        let Mode::Prompt(prompt) = &self.mode else {
            return;
        };
        let PromptAction::Search { origin } = prompt.action else {
            return;
        };
        self.search = prompt.input.clone();

        // Every keystroke searches afresh from where the search began, so a query
        // typed one letter too far leaves the cursor where it started rather than
        // somewhere the query no longer describes.
        self.selected = origin;
        self.select_match(indices_from(self.entries.len(), origin));
    }

    /// Puts the cursor on the first row in `order` that matches the search.
    fn select_match(&mut self, mut order: impl Iterator<Item = usize>) {
        if self.search.is_empty() {
            return;
        }

        let found = order.find(|&index| matches(&self.entries[index].name, &self.search));
        if let Some(index) = found {
            self.selected = index;
        }
    }

    /// Quits when the file was handed to neovim: nav has done its job.
    ///
    /// That quit never moves the shell — only `Q` does — so opening a file from
    /// somewhere you were only passing through leaves the shell where it was.
    pub fn enter_selected(&mut self) -> Resultx<Outcome> {
        if self.entries.is_empty() {
            return Ok(Outcome::Stay);
        }

        let entry = &self.entries[self.selected];
        let name = entry.name.clone();

        if !entry.is_dir {
            let opened = nvim::open(&self.current_dir.join(&name))?;
            return Ok(if opened { Outcome::Quit } else { Outcome::Stay });
        }

        match name.as_str() {
            "." => log::info!(
                "Staying in the same directory: {}",
                self.current_dir.display()
            ),
            ".." => self.go_to_parent_directory()?,
            name => {
                let new_path = self.current_dir.join(name);
                log::info!("Entering directory: {}", new_path.display());
                self.go_to(new_path)?;
            }
        }
        Ok(Outcome::Stay)
    }

    /// Goes up one level. At the filesystem root there is nowhere to go, so this does nothing.
    pub fn go_to_parent_directory(&mut self) -> Resultx<()> {
        if let Some(parent) = self.current_dir.parent() {
            log::info!("Going to parent: {}", parent.display());
            self.go_to(parent.to_path_buf())?;
        }
        Ok(())
    }

    /// Switches to `path`, saving the cursor position here and restoring it there.
    ///
    /// Every directory change goes through this, so saving and restoring happen in
    /// one place rather than at each call site.
    fn go_to(&mut self, path: PathBuf) -> Resultx<()> {
        let entries = dir::read_dir_with_dots(&path)?;

        if let Some(entry) = self.entries.get(self.selected) {
            self.memory.remember(&self.current_dir, &entry.name);
        }

        // Only reached when `path` has never been visited: on the way up out of a
        // directory we started in, that directory is still where we just were.
        let came_from = self
            .current_dir
            .strip_prefix(&path)
            .ok()
            .and_then(|below| below.iter().next())
            .map(|name| name.to_string_lossy().into_owned());

        let selected = [self.memory.recall(&path), came_from.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|name| index_of(&entries, name))
            .unwrap_or(0);

        self.entries = entries;
        self.current_dir = path;
        self.selected = selected;
        self.scroll_offset = 0;
        self.cached_image = None;
        Ok(())
    }

    /// The entry under the cursor, unless it is one of the `.` / `..` shortcuts,
    /// which name directories that operations must never be pointed at.
    fn cursor_path(&self) -> Option<PathBuf> {
        let entry = self.entries.get(self.selected)?;
        let navigational = entry.name == "." || entry.name == "..";
        (!navigational).then(|| self.current_dir.join(&entry.name))
    }

    /// What a staging key acts on: everything marked, or the entry under the
    /// cursor when nothing is. Single-file work should not need a mark first.
    fn targets(&self) -> Vec<PathBuf> {
        if self.marks.is_empty() {
            return self.cursor_path().into_iter().collect();
        }
        self.marks.iter().cloned().collect()
    }

    fn toggle_mark(&mut self) {
        let Some(path) = self.cursor_path() else {
            return;
        };

        if self.marks.toggle(&path) {
            log::info!("Marked {}", path.display());
        } else {
            log::info!("Unmarked {}", path.display());
        }
    }

    /// Undoes one intention at a time: the search first, then the marks, then the
    /// staged plan.
    ///
    /// None of them has touched the disk, so all three are only intentions — they
    /// come off least deliberate first, so a reflexive esc after a search cannot
    /// take a plan with it. Unlike vim's `:nohlsearch`, the search itself goes and
    /// not just its highlight: one esc, one thing gone, and `n` has nothing left to
    /// repeat.
    fn back_out(&mut self) {
        if !self.search.is_empty() {
            log::info!("Cleared the search for {:?}", self.search);
            self.search.clear();
        } else if !self.marks.is_empty() {
            log::info!("Cleared {} marks", self.marks.len());
            self.marks.clear();
        } else if !self.plan.is_empty() {
            log::info!("Discarded {} staged operation(s)", self.plan.len());
            self.plan.clear();
        }
    }

    /// Stages one operation per target, landing in the directory being viewed.
    fn stage_into_here(&mut self, build: impl Fn(PathBuf, PathBuf) -> Op) {
        let mut staged = 0;
        for from in self.targets() {
            let Some(name) = from.file_name() else {
                continue;
            };
            let to = self.current_dir.join(name);
            if self.plan.push(build(from, to)) {
                staged += 1;
            }
        }
        log::info!(
            "Staged {staged} operation(s) into {}",
            self.current_dir.display()
        );
    }

    fn stage_trash(&mut self) {
        let mut staged = 0;
        for path in self.targets() {
            if self.plan.push(Op::Trash(path)) {
                staged += 1;
            }
        }
        log::info!("Staged {staged} for the trash");
    }

    fn begin_prompt(&mut self, label: &'static str, action: PromptAction) {
        self.mode = Mode::Prompt(Prompt {
            label,
            input: String::new(),
            action,
        });
    }

    fn begin_rename(&mut self) {
        let Some(path) = self.cursor_path() else {
            return;
        };

        let current = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        self.mode = Mode::Prompt(Prompt {
            label: "rename to",
            input: current,
            action: PromptAction::Rename(path),
        });
    }

    fn begin_retarget(&mut self) {
        let Some(staged) = self.plan.get_mut(self.review_selected) else {
            return;
        };

        let Some(destination) = staged.op.destination_mut() else {
            log::warn!("a trash has no destination to rename");
            return;
        };

        let current = destination
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        self.mode = Mode::Prompt(Prompt {
            label: "destination name",
            input: current,
            action: PromptAction::Retarget(self.review_selected),
        });
    }

    fn confirm_prompt(&mut self) {
        // Taken out so the plan can be edited; put back if the name is unusable.
        let Mode::Prompt(prompt) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };

        // Neither of these types a file name, so neither goes through the name
        // check that refuses slashes: a search has already moved the cursor as it
        // was typed, and a jump hands its words to zoxide.
        match prompt.action {
            PromptAction::Search { .. } => return,
            PromptAction::Jump => {
                self.jump(prompt.input.trim());
                return;
            }
            _ => {}
        }

        let name = prompt.input.trim().to_string();
        if let Some(problem) = validate_name(&name) {
            log::warn!("{name:?} is {problem}");
            self.mode = Mode::Prompt(prompt);
            return;
        }

        match prompt.action {
            PromptAction::CreateFile => {
                self.plan.push(Op::CreateFile(self.current_dir.join(name)));
            }
            PromptAction::CreateDir => {
                self.plan.push(Op::CreateDir(self.current_dir.join(name)));
            }
            PromptAction::Rename(from) => {
                let to = self.current_dir.join(name);
                self.plan.push(Op::Move { from, to });
            }
            PromptAction::Retarget(index) => {
                if let Some(staged) = self.plan.get_mut(index) {
                    staged.op.retarget(&name);
                }
                self.mode = Mode::Review;
            }
            // Both returned above, before the name check they must not reach.
            PromptAction::Jump | PromptAction::Search { .. } => {}
        }
    }

    /// Goes wherever zoxide ranks `query` highest.
    ///
    /// A query it does not know, or one whose directory has since been removed,
    /// leaves the navigator where it is with the reason on the status line.
    fn jump(&mut self, query: &str) {
        if query.is_empty() {
            return;
        }

        let jumped = zoxide::query(query).and_then(|path| {
            log::info!("Jumping to {}", path.display());
            self.go_to(path)
        });

        if let Err(e) = jumped {
            log::warn!("{e}");
        }
    }

    fn open_review(&mut self) {
        if self.plan.is_empty() {
            log::info!("Nothing staged");
            return;
        }
        self.clamp_review();
        self.mode = Mode::Review;
    }

    fn clamp_review(&mut self) {
        self.review_selected = self.review_selected.min(self.plan.len().saturating_sub(1));
    }

    /// Runs every staged operation in order, keeping going past any that fail.
    ///
    /// A filesystem has no transactions, so stopping at the first failure would
    /// leave a half-applied plan with no account of which half.
    fn apply_plan(&mut self) -> Resultx<()> {
        if self.plan.ops().any(|op| fs_ops::problem(op).is_some()) {
            log::warn!("Blocked operations in the plan - drop them with x or X");
            return Ok(());
        }

        let ops: Vec<Op> = self.plan.ops().cloned().collect();
        let mut failures = Vec::new();

        for op in ops {
            if let Err(e) = fs_ops::apply(&op) {
                log::error!("{e}");
                failures.push((op, e.to_string()));
            }
        }

        let failed = failures.len();
        self.plan.keep_failures(failures);
        self.marks.clear();
        self.mode = if failed == 0 {
            Mode::Normal
        } else {
            Mode::Review
        };
        self.clamp_review();
        self.refresh()?;

        if failed == 0 {
            log::info!("Plan applied");
        } else {
            log::error!("{failed} operation(s) failed and are still staged");
        }
        Ok(())
    }

    fn yank_to_clipboard(&self) {
        let Some(path) = self.cursor_path() else {
            return;
        };

        match clipboard::copy_file(&path) {
            Ok(bytes) => log::info!("Copied {bytes} bytes to the clipboard"),
            Err(e) => log::warn!("{e}"),
        }
    }

    /// Re-reads the current directory, keeping the cursor on the same entry when
    /// it is still there.
    ///
    /// The directory itself can be gone — trashed from inside it, or removed by
    /// something else — which leaves nowhere to stand, so that falls back to the
    /// nearest ancestor still there rather than showing a listing of a directory
    /// that no longer exists.
    fn refresh(&mut self) -> Resultx<()> {
        let Ok(entries) = dir::read_dir_with_dots(&self.current_dir) else {
            log::warn!("{} is gone", self.current_dir.display());
            return self.retreat();
        };

        let under_cursor = self
            .entries
            .get(self.selected)
            .map(|entry| entry.name.clone());

        self.selected = under_cursor
            .and_then(|name| index_of(&entries, &name))
            .unwrap_or(0);
        self.entries = entries;
        self.cached_image = None;
        Ok(())
    }

    /// Goes to the nearest ancestor that can still be read.
    fn retreat(&mut self) -> Resultx<()> {
        let gone = self.current_dir.clone();
        for ancestor in gone.ancestors().skip(1) {
            if self.go_to(ancestor.to_path_buf()).is_ok() {
                return Ok(());
            }
        }
        Err(Errx::any(format!(
            "nothing above {} can be read",
            gone.display()
        )))
    }

    pub fn move_up(&mut self) {
        self.move_up_by(1);
    }

    pub fn move_down(&mut self) {
        self.move_down_by(1);
    }

    pub fn move_up_by(&mut self, n: usize) {
        if self.log_panel_visible {
            // Scroll up in log panel (showing older entries)
            if let Some(store) = LOG_STORE.get() {
                let total = store.len();
                // Since we render from bottom, "up" means scroll to see older (earlier in list).
                self.log_scroll_offset = (self.log_scroll_offset + n).min(total.saturating_sub(1));
            }
        } else {
            self.selected = self.selected.saturating_sub(n);
            self.cached_image = None;
        }
    }

    pub fn move_down_by(&mut self, n: usize) {
        if self.log_panel_visible {
            // Scroll down in log panel (showing newer entries)
            self.log_scroll_offset = self.log_scroll_offset.saturating_sub(n);
        } else if !self.entries.is_empty() {
            self.selected = (self.selected + n).min(self.entries.len() - 1);
            self.cached_image = None;
        }
    }

    pub const fn toggle_log_panel(&mut self) {
        self.log_panel_visible = !self.log_panel_visible;
        self.log_scroll_offset = 0;
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if matches!(self.mode, Mode::Help) {
            render_help(area, buf);
            return;
        }

        if matches!(self.mode, Mode::Review) {
            self.render_review(area, buf);
            return;
        }

        // The listing, the staged plan when there is one, then one line that is
        // either what you are typing or the latest message. That last line is
        // drawn whatever else is on screen: a prompt you cannot see is one you
        // are typing at blind.
        let plan_rows = if self.plan.is_empty() {
            0
        } else {
            u16::try_from(self.plan.len().min(PLAN_PANEL_ROWS) + 1).unwrap_or(u16::MAX)
        };

        let chunks = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(plan_rows),
            Constraint::Length(2),
        ])
        .split(area);

        if self.log_panel_visible {
            // The log takes the room the preview had, the listing keeps the top.
            let split = Layout::vertical([Constraint::Percentage(30), Constraint::Percentage(70)])
                .split(chunks[0]);

            self.render_file_list(split[0], buf);
            self.render_log_panel(split[1], buf);
        } else {
            self.render_with_preview(chunks[0], buf);
        }

        if plan_rows > 0 {
            self.render_plan_panel(chunks[1], buf);
        }

        match &self.mode {
            Mode::Prompt(prompt) => render_prompt(prompt, chunks[2], buf),
            _ => self.render_status_line(chunks[2], buf),
        }
    }

    fn render_plan_panel(&self, area: Rect, buf: &mut Buffer) {
        let hidden = self.plan.len().saturating_sub(PLAN_PANEL_ROWS);
        let title = if hidden == 0 {
            format!(" plan ({}) ", self.plan.len())
        } else {
            format!(" plan ({}, {hidden} more) ", self.plan.len())
        };

        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(DIM))
            .title(Line::styled(title, Style::new().fg(DIM)));
        let inner = block.inner(area);
        block.render(area, buf);

        for (i, staged) in self.plan.iter().take(inner.height as usize).enumerate() {
            plan_row(staged, false, inner.width).render(row(inner, i), buf);
        }
    }

    fn render_review(&self, area: Rect, buf: &mut Buffer) {
        let chunks = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

        let blocked = self
            .plan
            .ops()
            .filter(|op| fs_ops::problem(op).is_some())
            .count();

        Line::from(vec![
            Span::styled(" review", Style::new().fg(ACCENT).bold()),
            Span::styled(
                format!("  ·  {} staged", self.plan.len()),
                Style::new().fg(DIM),
            ),
        ])
        .render(chunks[0], buf);

        // Scrolled just far enough to keep the selected row on screen: `x` drops
        // what the cursor is on, so the cursor must never be off the bottom.
        let rows = chunks[1];
        let height = rows.height as usize;
        let first = (self.review_selected + 1).saturating_sub(height);

        for (i, staged) in self.plan.iter().skip(first).take(height).enumerate() {
            plan_row(staged, first + i == self.review_selected, rows.width)
                .render(row(rows, i), buf);
        }

        let mut footer = vec![Span::raw(" ")];
        if blocked > 0 {
            footer.push(Span::styled(
                format!("{blocked} blocked"),
                Style::new().fg(Color::Red).bold(),
            ));
            footer.push(Span::styled("  ·  ", Style::new().fg(DIM)));
        }
        footer.extend(hints(&[
            ("enter", "apply"),
            ("x", "drop"),
            ("X", "drop blocked"),
            ("r", "rename"),
            ("esc", "back"),
        ]));
        Line::from(footer).render(chunks[2], buf);
    }

    fn render_status_line(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default().borders(Borders::TOP);
        let inner = block.inner(area);
        block.render(area, buf);

        let latest = LOG_STORE.get().and_then(|store| {
            let entry = store.latest()?;
            let age = store
                .time_since_start()
                .saturating_sub(store.elapsed_since(&entry))
                .as_secs();
            Some((entry, age))
        });

        let mut spans = Vec::new();

        if !self.marks.is_empty() {
            let elsewhere = self.marks.count_outside(&self.current_dir);
            let summary = if elsewhere == 0 {
                format!("{} marked", self.marks.len())
            } else {
                format!("{} marked ({elsewhere} elsewhere)", self.marks.len())
            };
            spans.push(Span::styled(summary, Style::new().fg(MARK).bold()));
            if latest.is_some() {
                spans.push(Span::styled("  ·  ", Style::new().fg(DIM)));
            }
        }

        if let Some((entry, seconds)) = latest {
            let age = if seconds < 60 {
                format!("{seconds}s")
            } else {
                format!("{}m", seconds / 60)
            };

            spans.extend(log_spans(&entry));
            spans.push(Span::styled(format!("  {age}"), Style::new().fg(DIM)));
        }

        Line::from(spans).render(inner, buf);
    }

    fn render_with_preview(&mut self, area: Rect, buf: &mut Buffer) {
        let chunks = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area);

        self.render_file_list(chunks[0], buf);
        // A column of air either side, so the preview does not touch the divider.
        self.render_preview(chunks[1].inner(Margin::new(1, 0)), buf);
    }

    fn render_file_list(&mut self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::new().fg(DIM))
            .title(path_title(&self.current_dir, area.width));

        let inner = block.inner(area);
        block.render(area, buf);

        let visible_height = inner.height as usize;
        let scrolloff = SCROLL_OFF.min(visible_height / 2);

        // Keep the cursor on screen with `scrolloff` rows of context beyond it,
        // but never scroll past the last entry: the context the end of a listing
        // has to offer is the end of the listing, not a screenful of blank rows.
        if self.selected < self.scroll_offset + scrolloff {
            self.scroll_offset = self.selected.saturating_sub(scrolloff);
        } else if self.selected + scrolloff >= self.scroll_offset + visible_height {
            self.scroll_offset = (self.selected + scrolloff + 1).saturating_sub(visible_height);
        }
        self.scroll_offset = self
            .scroll_offset
            .min(self.entries.len().saturating_sub(visible_height));

        for (i, entry) in self
            .entries
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(visible_height)
        {
            let color = file_color(&entry.name, entry.is_dir);
            let under_cursor = i == self.selected;
            let marked = self.marks.contains(&self.current_dir.join(&entry.name));

            let name = if under_cursor {
                Style::new().fg(color).bold()
            } else {
                Style::new().fg(color)
            };

            let mut spans = vec![
                Span::styled(
                    if under_cursor { CURSOR_BAR } else { " " },
                    Style::new().fg(ACCENT),
                ),
                Span::styled(if marked { MARK_DOT } else { " " }, Style::new().fg(MARK)),
                Span::raw(" "),
                Span::styled(icon_for(entry), Style::new().fg(color)),
                Span::raw(" "),
            ];
            spans.extend(name_spans(&entry.name, &self.search, name));

            Line::from(spans).render(row(inner, i - self.scroll_offset), buf);
        }
    }

    fn render_preview(&mut self, area: Rect, buf: &mut Buffer) {
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };

        let path = self.current_dir.join(&entry.name);
        match FileKind::of(&path, entry.is_dir) {
            FileKind::Dir => render_directory_preview(&path, area, buf),
            FileKind::Image => self.render_image_preview(&path, area, buf),
            FileKind::Text => render_text_preview(&path, area, buf),
        }
    }

    fn render_image_preview(&mut self, path: &Path, area: Rect, buf: &mut Buffer) {
        let cached = self
            .cached_image
            .as_ref()
            .is_some_and(|(cached, _)| cached == path);

        if !cached {
            match raster::load(path) {
                Ok(img) => {
                    let protocol = self.picker.new_resize_protocol(img);
                    self.cached_image = Some((path.to_path_buf(), protocol));
                }
                Err(e) => {
                    log::warn!("{e}");
                    preview_text("(cannot load image)".to_string()).render(area, buf);
                    return;
                }
            }
        }

        if let Some((_, protocol)) = &mut self.cached_image {
            StatefulImage::default().render(area, buf, protocol);
        }
    }

    fn render_log_panel(&mut self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(DIM));
        let inner = block.inner(area);
        block.render(area, buf);

        let Some(store) = LOG_STORE.get() else {
            return;
        };

        let entries = store.entries();
        let visible_height = inner.height as usize;

        // Keep the scroll within bounds as the panel resizes or entries expire.
        self.log_scroll_offset = self
            .log_scroll_offset
            .min(entries.len().saturating_sub(visible_height));

        // Newest at the bottom, so the first entry drawn goes on the last row.
        for (i, entry) in entries
            .iter()
            .rev()
            .skip(self.log_scroll_offset)
            .take(visible_height)
            .enumerate()
        {
            Line::from(log_spans(entry)).render(row(inner, visible_height - 1 - i), buf);
        }
    }
}

/// One staged operation, with whatever is wrong with it.
///
/// Shared by the plan panel and the review screen so a row cannot say two
/// different things about the same operation depending on where you look.
fn plan_row(staged: &Staged, selected: bool, width: u16) -> Line<'_> {
    let described = staged.op.describe();
    let cursor = if selected { CURSOR_BAR } else { " " };

    // A failure from the last apply outranks a problem: it is what actually happened.
    let status = staged.failure.clone().map_or_else(
        || fs_ops::problem(&staged.op).map(|problem| problem.to_string()),
        Some,
    );
    let status = status.map(|status| format!("  ✗ {status}"));

    let mut spans = vec![
        Span::styled(cursor, Style::new().fg(ACCENT)),
        Span::raw(" "),
        Span::styled(
            format!("{:<7}", described.verb),
            Style::new().fg(Color::Yellow),
        ),
        Span::raw(described.subject.clone()),
    ];

    // The name being acted on and the reason a row is blocked both stay whole;
    // the destination path is the only part with width to give up.
    if let Some(destination) = described.destination {
        let spent = 2
            + 7
            + described.subject.chars().count()
            + 3
            + status.as_ref().map_or(0, |s| s.chars().count());
        let room = (width as usize).saturating_sub(spent);

        spans.push(Span::styled(" → ", Style::new().fg(DIM)));
        spans.push(Span::raw(truncate(&shorten_home(&destination), room)));
    }

    if let Some(status) = status {
        spans.push(Span::styled(status, Style::new().fg(Color::Red)));
    }

    Line::from(spans)
}

/// Keeps the tail of `text`, which for a path is the part that identifies it.
fn truncate(text: &str, width: usize) -> String {
    let length = text.chars().count();
    if length <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".repeat(width);
    }
    let skipped = length - (width - 1);
    std::iter::once('…')
        .chain(text.chars().skip(skipped))
        .collect()
}

/// The key map, laid out top to bottom and wrapping into as many columns as the
/// terminal's height needs, so nothing is ever cut off.
fn render_help(area: Rect, buf: &mut Buffer) {
    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);

    Line::from(vec![
        Span::styled(" nav", Style::new().fg(ACCENT).bold()),
        Span::styled("  ·  keys", Style::new().fg(DIM)),
    ])
    .render(chunks[0], buf);

    let body = chunks[1];
    let columns = help_columns(body.height as usize);
    let count = u32::try_from(columns.len()).unwrap_or(1).max(1);
    let areas = Layout::horizontal(vec![Constraint::Ratio(1, count); count as usize]).split(body);

    for (column, area) in columns.into_iter().zip(areas.iter()) {
        for (i, line) in column.into_iter().enumerate() {
            line.render(row(*area, i), buf);
        }
    }

    Line::styled(" press any key to go back", Style::new().fg(DIM)).render(chunks[2], buf);
}

/// Packs the sections into columns at most `height` rows tall.
///
/// A section is never split across two columns: half a group of keys stranded at
/// the bottom of one column reads as a different group than it is. A section
/// taller than the whole screen has no whole column to be kept in, and spills
/// into the next one rather than over what is drawn below the help.
fn help_columns(height: usize) -> Vec<Vec<Line<'static>>> {
    let height = height.max(1);
    let mut columns = Vec::new();
    let mut current: Vec<Line<'static>> = Vec::new();

    for section in HELP {
        let lines = section_lines(section);

        if !current.is_empty() {
            if current.len() + 1 + lines.len() > height {
                columns.push(std::mem::take(&mut current));
            } else {
                current.push(Line::raw(""));
            }
        }
        current.extend(lines);

        while current.len() > height {
            let spilled = current.split_off(height);
            columns.push(std::mem::replace(&mut current, spilled));
        }
    }

    if !current.is_empty() {
        columns.push(current);
    }
    columns
}

fn section_lines(section: &HelpSection) -> Vec<Line<'static>> {
    // One width across every section, so the columns line up with each other.
    let key_width = HELP
        .iter()
        .flat_map(|section| section.keys)
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0);

    let title = Line::styled(
        format!(" {}", section.title),
        Style::new().fg(Color::Yellow).bold(),
    );

    let keys = section.keys.iter().map(|(key, what)| {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{key:<key_width$}"), Style::new().fg(ACCENT)),
            Span::raw("  "),
            Span::raw(*what),
        ])
    });

    std::iter::once(title).chain(keys).collect()
}

fn render_prompt(prompt: &Prompt, area: Rect, buf: &mut Buffer) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(DIM));
    let inner = block.inner(area);
    block.render(area, buf);

    Line::from(vec![
        Span::styled(prompt.label, Style::new().fg(DIM)),
        Span::styled(" › ", Style::new().fg(ACCENT).bold()),
        Span::raw(&prompt.input),
        Span::styled("▏", Style::new().fg(ACCENT)),
    ])
    .render(inner, buf);
}

/// The directory being viewed, with `$HOME` shortened and everything above the
/// last component dimmed, so the name of the directory you are in is what reads.
fn path_title(dir: &Path, width: u16) -> Line<'static> {
    let shown = shorten_home(&dir.to_string_lossy());
    // Keeping the tail matters: the directory you are in is the last component.
    let shown = truncate(&shown, (width as usize).saturating_sub(2));
    let split = shown.rfind('/').map_or(0, |at| at + 1);

    Line::from(vec![
        Span::raw(" "),
        Span::styled(shown[..split].to_string(), Style::new().fg(DIM)),
        Span::styled(shown[split..].to_string(), Style::new().bold()),
        Span::raw(" "),
    ])
}

fn shorten_home(path: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return path.to_string();
    };

    let home = home.to_string_lossy();
    match path.strip_prefix(home.as_ref()) {
        // A whole component only: `/home/ioannis` must not become `~nis`.
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// A log entry as a coloured dot and its message, shared by the status line and
/// the log panel so one cannot drift from the other.
fn log_spans(entry: &LogEntry) -> Vec<Span<'static>> {
    vec![
        Span::styled("●", Style::new().fg(level_color(entry.level))),
        Span::raw(" "),
        Span::raw(entry.message.clone()),
    ]
}

/// `key description` pairs for a footer, keys picked out and the rest receding.
fn hints(pairs: &[(&'static str, &'static str)]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (key, what) in pairs {
        if !spans.is_empty() {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(*key, Style::new().fg(ACCENT)));
        spans.push(Span::styled(format!(" {what}"), Style::new().fg(DIM)));
    }
    spans
}

/// Preview content, with the "(cannot read file)" stand-ins dimmed so they do not
/// read as the file's own first line.
fn preview_text(content: String) -> Paragraph<'static> {
    let placeholder = content.starts_with('(') && content.ends_with(')') && !content.contains('\n');

    let style = if placeholder {
        Style::new().fg(DIM).italic()
    } else {
        Style::new()
    };
    Paragraph::new(content).style(style)
}

/// Every row of a `len`-row listing once, from `from` and wrapping around the
/// end: the order a search walks it in, and reversed, the order `N` walks it.
fn indices_from(len: usize, from: usize) -> impl DoubleEndedIterator<Item = usize> {
    (0..len).map(move |offset| (from + offset) % len)
}

/// Whether `name` contains `query`, ignoring case until the query has a capital
/// in it — vim's smartcase, and the same reflex applied to a listing.
fn matches(name: &str, query: &str) -> bool {
    !match_ranges(name, query).is_empty()
}

/// Every place `query` occurs in `name`, as byte ranges into `name`.
///
/// The cursor and the highlight both read this, so a row can never be jumped to
/// without lighting up, or lit up without being findable.
fn match_ranges(name: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }

    if query.chars().any(char::is_uppercase) {
        return name
            .match_indices(query)
            .map(|(at, found)| at..at + found.len())
            .collect();
    }

    // The query has no capitals to fold, so only the name needs lowering — but
    // lowering a character can change how wide it is, and one can even become
    // two ("İ" lowers to an i and a combining dot), so where a match lands in the
    // lowered copy is not where it lands in the name. `origins` carries each
    // lowered byte back to the character it came from, whole: a match that covers
    // any part of a character lights all of it, so a row can never be jumped to
    // without lighting up.
    let mut lowered = String::with_capacity(name.len());
    let mut origins: Vec<Range<usize>> = Vec::with_capacity(name.len());
    for (at, character) in name.char_indices() {
        lowered.extend(character.to_lowercase());
        origins.resize(lowered.len(), at..at + character.len_utf8());
    }

    lowered
        .match_indices(query)
        .map(|(at, found)| origins[at].start..origins[at + found.len() - 1].end)
        .collect()
}

/// `name`, cut into the parts the search matched and the parts it did not, so the
/// matches can be lit — vim's hlsearch, over a listing.
fn name_spans(name: &str, query: &str, style: Style) -> Vec<Span<'static>> {
    let lit = Style::new().bg(SEARCH).fg(SEARCH_TEXT);
    let mut spans = Vec::new();
    let mut at = 0;

    for Range { start, end } in match_ranges(name, query) {
        spans.push(Span::styled(name[at..start].to_string(), style));
        spans.push(Span::styled(name[start..end].to_string(), lit));
        at = end;
    }
    spans.push(Span::styled(name[at..].to_string(), style));
    spans
}

/// Whether anything beyond shift was held: shift is how a capital is typed, so it
/// is part of the letter rather than a modifier on it.
const fn held(key: KeyEvent) -> bool {
    !key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}

fn index_of(entries: &[DirEntry], name: &str) -> Option<usize> {
    entries.iter().position(|entry| entry.name == name)
}

/// Row `n` of `area`, counted from its top.
fn row(area: Rect, n: usize) -> Rect {
    let y = i32::try_from(n).unwrap_or(i32::MAX);
    area.offset(Offset { x: 0, y })
}

const fn icon_for(entry: &DirEntry) -> &'static str {
    if entry.is_dir {
        NF_OCT_FILE_DIRECTORY_FILL
    } else {
        " "
    }
}

fn render_directory_preview(path: &Path, area: Rect, buf: &mut Buffer) {
    let content = match dir::read_dir(path) {
        Ok(entries) if entries.is_empty() => "(empty directory)".to_string(),
        Ok(entries) => entries
            .iter()
            .take(area.height as usize)
            .map(|entry| format!("{}  {}", icon_for(entry), entry.name))
            .collect::<Vec<_>>()
            .join("\n"),
        Err(_) => "(cannot read directory)".to_string(),
    };

    preview_text(content).render(area, buf);
}

fn render_text_preview(path: &Path, area: Rect, buf: &mut Buffer) {
    let content = file::read_text_preview(path, area.height as usize);
    preview_text(content).render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::testdir::TempDir;

    fn open(path: &Path, select: Option<&str>) -> Navigator {
        Navigator::new(path.to_str().unwrap(), select).unwrap()
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    fn render_to_string(nav: &mut Navigator, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        nav.render(area, &mut buf);
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The name under the cursor.
    fn selected(nav: &Navigator) -> &str {
        &nav.entries[nav.selected].name
    }

    /// What is drawn on the search highlight, one entry per row that has any.
    fn lit(nav: &mut Navigator, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        nav.render(area, &mut buf);
        (0..height)
            .map(|y| {
                (0..width)
                    .filter(|&x| buf[(x, y)].bg == SEARCH)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .filter(|row| !row.is_empty())
            .collect()
    }

    #[test]
    fn renders_the_listing_and_the_preview_side_by_side() {
        let tmp = TempDir::new();
        tmp.dir("subdir");
        tmp.file("notes.txt", "the file contents");

        let mut nav = open(tmp.path(), Some("notes.txt"));
        let out = render_to_string(&mut nav, 80, 24);

        assert!(out.contains("subdir"), "missing directory in:\n{out}");
        assert!(out.contains("notes.txt"), "missing file in:\n{out}");
        assert!(
            out.contains("the file contents"),
            "missing preview in:\n{out}"
        );
    }

    #[test]
    fn starts_on_the_selected_file() {
        let tmp = TempDir::new();
        tmp.file("aaa.txt", "");
        tmp.file("zzz.txt", "");

        let nav = open(tmp.path(), Some("zzz.txt"));

        assert_eq!(selected(&nav), "zzz.txt");
    }

    #[test]
    fn starts_at_the_top_when_the_selection_is_missing() {
        let tmp = TempDir::new();
        tmp.file("aaa.txt", "");

        let nav = open(tmp.path(), Some("not-here.txt"));

        assert_eq!(selected(&nav), ".");
    }

    #[test]
    fn opening_a_missing_directory_fails() {
        let tmp = TempDir::new();

        assert!(Navigator::new(tmp.path().join("nope").to_str().unwrap(), None).is_err());
    }

    #[test]
    fn entering_a_subdirectory_descends_into_it() {
        let tmp = TempDir::new();
        tmp.file("sub/inside.txt", "");

        let mut nav = open(tmp.path(), Some("sub"));
        nav.enter_selected().unwrap();

        assert_eq!(nav.current_dir, tmp.path().join("sub"));
        assert_eq!(selected(&nav), ".");
        assert!(nav.entries.iter().any(|e| e.name == "inside.txt"));
    }

    #[test]
    fn entering_dot_dot_goes_up_instead_of_appending_to_the_path() {
        let tmp = TempDir::new();
        tmp.dir("sub");
        tmp.file("marker.txt", "");

        let mut nav = open(&tmp.path().join("sub"), None);
        nav.selected = 1;
        assert_eq!(selected(&nav), "..");
        nav.enter_selected().unwrap();

        assert_eq!(nav.current_dir, tmp.path());
        assert!(nav.entries.iter().any(|e| e.name == "marker.txt"));
    }

    #[test]
    fn entering_dot_stays_put() {
        let tmp = TempDir::new();
        tmp.dir("sub");

        let mut nav = open(tmp.path(), None);
        assert_eq!(selected(&nav), ".");
        nav.enter_selected().unwrap();

        assert_eq!(nav.current_dir, tmp.path());
    }

    #[cfg(unix)]
    #[test]
    fn entering_a_symlinked_directory_descends_into_it() {
        let tmp = TempDir::new();
        let target = tmp.dir("real");
        tmp.file("real/inside.txt", "");
        std::os::unix::fs::symlink(&target, tmp.path().join("link")).unwrap();

        let mut nav = open(tmp.path(), Some("link"));
        nav.enter_selected().unwrap();

        assert!(nav.entries.iter().any(|e| e.name == "inside.txt"));
    }

    #[test]
    fn going_up_highlights_the_directory_you_came_from() {
        let tmp = TempDir::new();
        tmp.dir("aaa");
        tmp.dir("target");
        tmp.dir("zzz");

        let mut nav = open(tmp.path(), Some("target"));
        nav.enter_selected().unwrap();
        nav.go_to_parent_directory().unwrap();

        assert_eq!(selected(&nav), "target");
    }

    #[test]
    fn going_up_highlights_the_directory_you_started_in() {
        // Started here by `nav <path>`, so the parent has never been visited and
        // there is nothing remembered about it.
        let tmp = TempDir::new();
        tmp.dir("aaa");
        tmp.dir("start");

        let mut nav = open(&tmp.path().join("start"), None);
        nav.go_to_parent_directory().unwrap();

        assert_eq!(nav.current_dir, tmp.path());
        assert_eq!(selected(&nav), "start");
    }

    #[test]
    fn returning_to_a_directory_restores_the_cursor() {
        let tmp = TempDir::new();
        tmp.dir("sub");
        tmp.file("aaa.txt", "");
        tmp.file("zzz.txt", "");

        // Leave the cursor on a file, not on the directory we descend into.
        let mut nav = open(tmp.path(), Some("zzz.txt"));
        nav.go_to(tmp.path().join("sub")).unwrap();
        assert_eq!(selected(&nav), ".");

        nav.go_to_parent_directory().unwrap();

        assert_eq!(selected(&nav), "zzz.txt");
    }

    #[test]
    fn a_remembered_position_survives_several_levels() {
        let tmp = TempDir::new();
        tmp.dir("a/b/c");
        tmp.file("a/marker.txt", "");

        let mut nav = open(tmp.path(), Some("a"));
        nav.enter_selected().unwrap();
        nav.selected = index_of(&nav.entries, "marker.txt").unwrap();
        nav.go_to(tmp.path().join("a/b")).unwrap();
        nav.enter_selected().unwrap(); // into `.`, stays put

        nav.go_to_parent_directory().unwrap();

        assert_eq!(nav.current_dir, tmp.path().join("a"));
        assert_eq!(selected(&nav), "marker.txt");
    }

    #[test]
    fn a_remembered_entry_that_is_gone_falls_back_to_where_you_came_from() {
        let tmp = TempDir::new();
        tmp.dir("sub");
        let doomed = tmp.file("doomed.txt", "");

        let mut nav = open(tmp.path(), Some("doomed.txt"));
        nav.go_to(tmp.path().join("sub")).unwrap();
        std::fs::remove_file(&doomed).unwrap();

        nav.go_to_parent_directory().unwrap();

        assert_eq!(selected(&nav), "sub");
    }

    #[test]
    fn a_remembered_entry_that_is_gone_falls_back_to_the_top() {
        let tmp = TempDir::new();
        tmp.dir("here");
        tmp.dir("elsewhere");
        let doomed = tmp.file("here/doomed.txt", "");

        // Sideways, so the directory we return from is not one of `here`'s entries.
        let mut nav = open(&tmp.path().join("here"), Some("doomed.txt"));
        nav.go_to(tmp.path().join("elsewhere")).unwrap();
        std::fs::remove_file(&doomed).unwrap();
        nav.go_to(tmp.path().join("here")).unwrap();

        assert_eq!(selected(&nav), ".");
    }

    #[test]
    fn a_directory_visited_for_the_first_time_starts_at_the_top() {
        let tmp = TempDir::new();
        tmp.file("sub/inside.txt", "");

        let mut nav = open(tmp.path(), Some("sub"));
        nav.enter_selected().unwrap();

        assert_eq!(selected(&nav), ".");
    }

    #[test]
    fn descending_does_not_inherit_the_parents_position() {
        let tmp = TempDir::new();
        tmp.file("aaa.txt", "");
        tmp.file("sub/aaa.txt", "");

        // `aaa.txt` exists in both, so a naive restore would land on it below too.
        let mut nav = open(tmp.path(), Some("aaa.txt"));
        nav.go_to(tmp.path().join("sub")).unwrap();

        assert_eq!(selected(&nav), ".");
    }

    #[test]
    fn q_quits_and_leaves_the_shell_where_it_was() {
        let tmp = TempDir::new();
        let mut nav = open(tmp.path(), None);

        let outcome = nav.handle_key(KeyEvent::from(KeyCode::Char('q')));

        assert_eq!(outcome, Outcome::Quit);
    }

    #[test]
    fn shift_q_quits_and_takes_the_shell_along() {
        let tmp = TempDir::new();
        tmp.dir("sub");

        let mut nav = open(tmp.path(), Some("sub"));
        key(&mut nav, KeyCode::Enter);
        let outcome = nav.handle_key(KeyEvent::from(KeyCode::Char('Q')));

        assert_eq!(outcome, Outcome::QuitHere);
        assert_eq!(
            nav.current_dir(),
            tmp.path().join("sub"),
            "the directory handed over is the one the session ended in"
        );
    }

    #[test]
    fn both_quits_work_from_the_review_screen_too() {
        let tmp = TempDir::new();
        tmp.file("x.txt", "");

        let mut nav = open(tmp.path(), Some("x.txt"));
        press(&mut nav, 'd');
        press(&mut nav, 'p');

        assert!(matches!(nav.mode, Mode::Review));
        assert_eq!(
            nav.handle_key(KeyEvent::from(KeyCode::Char('q'))),
            Outcome::Quit
        );
        assert_eq!(
            nav.handle_key(KeyEvent::from(KeyCode::Char('Q'))),
            Outcome::QuitHere
        );
    }

    fn press(nav: &mut Navigator, c: char) {
        nav.handle_key(KeyEvent::from(KeyCode::Char(c)));
    }

    fn key(nav: &mut Navigator, code: KeyCode) {
        nav.handle_key(KeyEvent::from(code));
    }

    fn type_name(nav: &mut Navigator, name: &str) {
        for c in name.chars() {
            press(nav, c);
        }
    }

    /// Open the review and apply, the way `enter` alone cannot: in the listing it
    /// still means "enter this directory".
    fn apply(nav: &mut Navigator) {
        press(nav, 'p');
        key(nav, KeyCode::Enter);
    }

    /// Puts the cursor on `name` and marks it.
    fn mark(nav: &mut Navigator, name: &str) {
        nav.selected = index_of(&nav.entries, name).unwrap_or_else(|| panic!("no {name} here"));
        press(nav, ' ');
    }

    /// Clears a prompt that was prefilled with the current name, then types.
    fn replace_name(nav: &mut Navigator, name: &str) {
        for _ in 0..128 {
            key(nav, KeyCode::Backspace);
        }
        type_name(nav, name);
    }

    #[test]
    fn a_path_that_fits_is_left_alone() {
        assert_eq!(truncate("/a/b/notes.txt", 40), "/a/b/notes.txt");
        assert_eq!(truncate("exactly-ten", 11), "exactly-ten");
    }

    #[test]
    fn a_path_too_long_keeps_its_tail_and_the_width_it_was_given() {
        // The tail is what identifies a path, and the row it goes in has no more
        // columns to give it.
        assert_eq!(truncate("/home/me/dev/notes.txt", 12), "…v/notes.txt");
        assert_eq!(truncate("/home/me/dev/notes.txt", 12).chars().count(), 12);
    }

    #[test]
    fn there_is_a_narrowest_a_path_can_get() {
        assert_eq!(truncate("/a/b/c", 1), "…");
        assert_eq!(truncate("/a/b/c", 0), "");
    }

    #[test]
    fn truncating_never_cuts_a_character_in_half() {
        // Every one of these is several bytes wide, so counting bytes would slice
        // one down the middle and panic.
        assert_eq!(truncate("🦀🦀🦀🦀", 3), "…🦀🦀");
        assert_eq!(truncate("ärgerlich", 4), "…ich");
    }

    #[test]
    fn the_title_picks_out_the_directory_you_are_in() {
        let title = path_title(Path::new("/home/me/dev/navigator"), 40);
        let spans: Vec<_> = title
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert_eq!(spans, [" ", "/home/me/dev/", "navigator", " "]);
        assert!(
            title.spans[2]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn a_narrow_title_still_ends_with_the_directory_you_are_in() {
        let title = path_title(Path::new("/home/me/dev/navigator"), 14);
        let shown: String = title
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert!(shown.trim().ends_with("navigator"), "{shown:?}");
        assert!(
            shown.chars().count() <= 15,
            "{shown:?} overflows 14 columns"
        );
    }

    #[test]
    fn question_mark_shows_the_keys() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '?');
        let out = render_to_string(&mut nav, 100, 30);

        for section in HELP {
            assert!(
                out.contains(section.title),
                "missing {} in:\n{out}",
                section.title
            );
        }
        assert!(out.contains("press any key to go back"), "in:\n{out}");
    }

    #[test]
    fn every_documented_key_reaches_the_screen() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '?');
        let out = render_to_string(&mut nav, 120, 40);

        for (key, what) in HELP.iter().flat_map(|section| section.keys) {
            assert!(out.contains(key), "key {key} was cut off in:\n{out}");
            assert!(out.contains(what), "text for {key} was cut off in:\n{out}");
        }
    }

    #[test]
    fn the_keys_fit_a_short_terminal_by_using_more_columns() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '?');
        // Far too short for one column, so it must wrap into several.
        let out = render_to_string(&mut nav, 200, 14);

        for section in HELP {
            assert!(
                out.contains(section.title),
                "missing {} in:\n{out}",
                section.title
            );
        }
    }

    #[test]
    fn no_column_of_keys_is_taller_than_the_screen() {
        // A section on its own is taller than this, so keeping every one whole
        // would mean drawing over whatever the help sits on.
        for height in 0..12 {
            for column in help_columns(height) {
                assert!(
                    column.len() <= height.max(1),
                    "a column of {} lines does not fit {height} rows",
                    column.len()
                );
            }
        }
    }

    #[test]
    fn no_key_is_documented_twice_in_one_section() {
        for section in HELP {
            for (i, (key, _)) in section.keys.iter().enumerate() {
                let duplicate = section
                    .keys
                    .iter()
                    .skip(i + 1)
                    .any(|(other, _)| other == key);
                assert!(!duplicate, "{key} is listed twice under {}", section.title);
            }
        }
    }

    #[test]
    fn any_key_dismisses_the_help() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");

        for dismiss in ['?', 'j', 'q', 'z'] {
            let mut nav = open(tmp.path(), None);
            press(&mut nav, '?');
            assert!(matches!(nav.mode, Mode::Help));

            press(&mut nav, dismiss);
            assert!(
                matches!(nav.mode, Mode::Normal),
                "{dismiss} should have closed the help"
            );
        }
    }

    #[test]
    fn dismissing_the_help_does_not_also_run_that_key() {
        let tmp = TempDir::new();
        tmp.file("victim.txt", "");

        let mut nav = open(tmp.path(), Some("victim.txt"));
        press(&mut nav, '?');
        press(&mut nav, 'd');

        assert!(
            nav.plan.is_empty(),
            "the key that closed the help must not also stage"
        );
    }

    #[test]
    fn marking_leaves_the_cursor_where_it_is() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");
        tmp.file("b.txt", "");

        let mut nav = open(tmp.path(), Some("a.txt"));
        press(&mut nav, ' ');

        assert!(nav.marks.contains(&tmp.path().join("a.txt")));
        assert_eq!(selected(&nav), "a.txt");
    }

    #[test]
    fn marking_the_same_entry_again_unmarks_it() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");

        let mut nav = open(tmp.path(), Some("a.txt"));
        press(&mut nav, ' ');
        press(&mut nav, ' ');

        assert!(nav.marks.is_empty());
    }

    #[test]
    fn the_navigation_shortcuts_cannot_be_marked() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");

        let mut nav = open(tmp.path(), None);
        assert_eq!(selected(&nav), ".");
        press(&mut nav, ' ');

        assert!(nav.marks.is_empty(), "`.` must never become a target");
    }

    #[test]
    fn escape_clears_every_mark() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");
        tmp.file("b.txt", "");

        let mut nav = open(tmp.path(), Some("a.txt"));
        mark(&mut nav, "a.txt");
        mark(&mut nav, "b.txt");
        assert_eq!(nav.marks.len(), 2);

        key(&mut nav, KeyCode::Esc);

        assert!(nav.marks.is_empty());
    }

    #[test]
    fn escape_discards_the_plan_once_the_marks_are_gone() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");
        tmp.dir("sub");

        let mut nav = open(tmp.path(), None);
        mark(&mut nav, "a.txt");
        nav.go_to(tmp.path().join("sub")).unwrap();
        press(&mut nav, 'c');

        // The marks go first: they are what the plan was built from, and losing
        // both to one keypress would be a surprise.
        key(&mut nav, KeyCode::Esc);
        assert!(nav.marks.is_empty());
        assert_eq!(nav.plan.len(), 1, "the plan should have survived");

        key(&mut nav, KeyCode::Esc);
        assert!(nav.plan.is_empty());
    }

    #[test]
    fn escape_with_nothing_to_back_out_of_does_nothing() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");

        let mut nav = open(tmp.path(), Some("a.txt"));
        key(&mut nav, KeyCode::Esc);

        assert!(nav.marks.is_empty());
        assert!(nav.plan.is_empty());
        assert_eq!(selected(&nav), "a.txt");
    }

    #[test]
    fn staging_with_nothing_marked_uses_the_cursor() {
        let tmp = TempDir::new();
        tmp.file("lonely.txt", "");

        let mut nav = open(tmp.path(), Some("lonely.txt"));
        press(&mut nav, 'd');

        assert_eq!(nav.plan.len(), 1);
        assert_eq!(
            nav.plan.ops().next().unwrap().describe().subject,
            "lonely.txt"
        );
    }

    #[test]
    fn marked_entries_are_copied_into_the_directory_you_are_standing_in() {
        let tmp = TempDir::new();
        tmp.file("one.txt", "first");
        tmp.file("two.txt", "second");
        tmp.dir("dest");

        let mut nav = open(tmp.path(), Some("one.txt"));
        mark(&mut nav, "one.txt");
        mark(&mut nav, "two.txt");

        nav.go_to(tmp.path().join("dest")).unwrap();
        press(&mut nav, 'c');
        apply(&mut nav);

        assert_eq!(read(&tmp.path().join("dest/one.txt")), "first");
        assert_eq!(read(&tmp.path().join("dest/two.txt")), "second");
        assert!(
            tmp.path().join("one.txt").exists(),
            "copy keeps the original"
        );
    }

    #[test]
    fn moving_marked_entries_removes_the_originals() {
        let tmp = TempDir::new();
        let source = tmp.file("travelling.txt", "contents");
        tmp.dir("dest");

        let mut nav = open(tmp.path(), Some("travelling.txt"));
        press(&mut nav, ' ');
        nav.go_to(tmp.path().join("dest")).unwrap();
        press(&mut nav, 'm');
        apply(&mut nav);

        assert_eq!(read(&tmp.path().join("dest/travelling.txt")), "contents");
        assert!(!source.exists());
    }

    #[test]
    fn staging_the_same_thing_twice_stages_it_once() {
        let tmp = TempDir::new();
        tmp.file("x.txt", "");
        tmp.dir("sub");

        let mut nav = open(tmp.path(), None);
        mark(&mut nav, "x.txt");
        nav.go_to(tmp.path().join("sub")).unwrap();
        press(&mut nav, 'c');
        press(&mut nav, 'c');
        press(&mut nav, 'd');
        press(&mut nav, 'd');

        // One copy and one trash, not two of each: the second of each pair would
        // only have failed on work the first had already done.
        assert_eq!(nav.plan.len(), 2);
    }

    #[test]
    fn marks_survive_staging_so_one_set_can_go_to_two_places() {
        let tmp = TempDir::new();
        tmp.file("shared.txt", "x");
        tmp.dir("first");
        tmp.dir("second");

        let mut nav = open(tmp.path(), Some("shared.txt"));
        press(&mut nav, ' ');

        nav.go_to(tmp.path().join("first")).unwrap();
        press(&mut nav, 'c');
        nav.go_to(tmp.path().join("second")).unwrap();
        press(&mut nav, 'c');

        assert_eq!(nav.plan.len(), 2);
        apply(&mut nav);

        assert!(tmp.path().join("first/shared.txt").exists());
        assert!(tmp.path().join("second/shared.txt").exists());
    }

    #[test]
    fn applying_clears_the_marks_and_empties_the_plan() {
        let tmp = TempDir::new();
        tmp.file("doomed.txt", "");

        let mut nav = open(tmp.path(), Some("doomed.txt"));
        press(&mut nav, ' ');
        press(&mut nav, 'd');
        apply(&mut nav);

        assert!(nav.marks.is_empty());
        assert!(nav.plan.is_empty());
        assert!(!tmp.path().join("doomed.txt").exists());
    }

    #[test]
    fn applying_refreshes_the_listing() {
        let tmp = TempDir::new();
        tmp.file("doomed.txt", "");
        tmp.file("keeper.txt", "");

        let mut nav = open(tmp.path(), Some("doomed.txt"));
        press(&mut nav, 'd');
        apply(&mut nav);

        assert!(
            !nav.entries.iter().any(|e| e.name == "doomed.txt"),
            "the trashed entry should be gone from the listing"
        );
        assert!(nav.entries.iter().any(|e| e.name == "keeper.txt"));
    }

    #[test]
    fn a_blocked_plan_refuses_to_apply() {
        let tmp = TempDir::new();
        tmp.file("source.txt", "new");
        tmp.file("dest/source.txt", "original");

        let mut nav = open(tmp.path(), Some("source.txt"));
        press(&mut nav, ' ');
        nav.go_to(tmp.path().join("dest")).unwrap();
        press(&mut nav, 'c');
        apply(&mut nav);

        assert_eq!(nav.plan.len(), 1, "the blocked operation must survive");
        assert_eq!(
            read(&tmp.path().join("dest/source.txt")),
            "original",
            "nothing may be clobbered"
        );
    }

    #[test]
    fn dropping_the_blocked_operations_lets_the_rest_through() {
        let tmp = TempDir::new();
        tmp.file("fresh.txt", "fresh");
        tmp.file("clash.txt", "new");
        tmp.file("dest/clash.txt", "original");

        let mut nav = open(tmp.path(), Some("clash.txt"));
        mark(&mut nav, "clash.txt");
        mark(&mut nav, "fresh.txt");
        nav.go_to(tmp.path().join("dest")).unwrap();
        press(&mut nav, 'c');
        assert_eq!(nav.plan.len(), 2);

        press(&mut nav, 'p');
        press(&mut nav, 'X');
        key(&mut nav, KeyCode::Enter);

        assert_eq!(read(&tmp.path().join("dest/clash.txt")), "original");
        assert_eq!(read(&tmp.path().join("dest/fresh.txt")), "fresh");
    }

    #[test]
    fn a_conflicting_destination_can_be_renamed_in_review() {
        let tmp = TempDir::new();
        tmp.file("notes.txt", "mine");
        tmp.file("dest/notes.txt", "theirs");

        let mut nav = open(tmp.path(), Some("notes.txt"));
        press(&mut nav, ' ');
        nav.go_to(tmp.path().join("dest")).unwrap();
        press(&mut nav, 'c');

        press(&mut nav, 'p');
        press(&mut nav, 'r');
        replace_name(&mut nav, "notes-mine.txt");
        key(&mut nav, KeyCode::Enter);
        key(&mut nav, KeyCode::Enter);

        assert_eq!(read(&tmp.path().join("dest/notes.txt")), "theirs");
        assert_eq!(read(&tmp.path().join("dest/notes-mine.txt")), "mine");
    }

    #[test]
    fn dropping_one_operation_leaves_the_others() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");
        tmp.file("b.txt", "");

        let mut nav = open(tmp.path(), Some("a.txt"));
        mark(&mut nav, "a.txt");
        mark(&mut nav, "b.txt");
        press(&mut nav, 'd');
        assert_eq!(nav.plan.len(), 2);

        press(&mut nav, 'p');
        press(&mut nav, 'x');

        assert_eq!(nav.plan.len(), 1);
    }

    #[test]
    fn creating_a_file_goes_through_the_plan() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, 'a');
        type_name(&mut nav, "fresh.rs");
        key(&mut nav, KeyCode::Enter);

        assert_eq!(nav.plan.len(), 1, "nothing should happen before applying");
        assert!(!tmp.path().join("fresh.rs").exists());

        apply(&mut nav);

        assert_eq!(read(&tmp.path().join("fresh.rs")), "");
    }

    #[test]
    fn creating_a_directory_goes_through_the_plan() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, 'A');
        type_name(&mut nav, "newdir");
        key(&mut nav, KeyCode::Enter);
        apply(&mut nav);

        assert!(tmp.path().join("newdir").is_dir());
    }

    #[test]
    fn renaming_starts_from_the_current_name() {
        let tmp = TempDir::new();
        tmp.file("original.txt", "");

        let mut nav = open(tmp.path(), Some("original.txt"));
        press(&mut nav, 'r');

        let Mode::Prompt(prompt) = &nav.mode else {
            panic!("expected a prompt");
        };
        assert_eq!(prompt.input, "original.txt");
    }

    #[test]
    fn renaming_applies_as_a_move() {
        let tmp = TempDir::new();
        let original = tmp.file("original.txt", "same bytes");

        let mut nav = open(tmp.path(), Some("original.txt"));
        press(&mut nav, 'r');
        replace_name(&mut nav, "renamed.txt");
        key(&mut nav, KeyCode::Enter);
        apply(&mut nav);

        assert!(!original.exists());
        assert_eq!(read(&tmp.path().join("renamed.txt")), "same bytes");
    }

    #[test]
    fn an_unusable_name_keeps_the_prompt_open() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, 'a');
        type_name(&mut nav, "in/valid");
        key(&mut nav, KeyCode::Enter);

        assert!(
            matches!(nav.mode, Mode::Prompt(_)),
            "a rejected name should not close the prompt"
        );
        assert!(nav.plan.is_empty());
    }

    #[test]
    fn escape_abandons_a_prompt() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, 'a');
        type_name(&mut nav, "unwanted.txt");
        key(&mut nav, KeyCode::Esc);

        assert!(matches!(nav.mode, Mode::Normal));
        assert!(nav.plan.is_empty());
    }

    /// Three entries a search can tell apart: `m1` and `m2` match "m", `other`
    /// matches neither.
    fn searchable() -> TempDir {
        let tmp = TempDir::new();
        tmp.file("m1.txt", "");
        tmp.file("m2.txt", "");
        tmp.file("other.txt", "");
        tmp
    }

    #[test]
    fn the_cursor_follows_the_search_as_it_is_typed() {
        let tmp = searchable();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '/');
        type_name(&mut nav, "m2");

        assert_eq!(selected(&nav), "m2.txt", "before enter was ever pressed");
    }

    #[test]
    fn enter_leaves_the_cursor_on_the_match() {
        let tmp = searchable();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '/');
        type_name(&mut nav, "m2");
        key(&mut nav, KeyCode::Enter);

        assert!(matches!(nav.mode, Mode::Normal));
        assert_eq!(selected(&nav), "m2.txt");
    }

    #[test]
    fn escape_puts_the_cursor_back_where_the_search_started() {
        let tmp = searchable();

        let mut nav = open(tmp.path(), Some("other.txt"));
        press(&mut nav, '/');
        type_name(&mut nav, "m1");
        key(&mut nav, KeyCode::Esc);

        assert!(matches!(nav.mode, Mode::Normal));
        assert_eq!(selected(&nav), "other.txt");
    }

    #[test]
    fn a_query_typed_one_letter_too_far_goes_back_to_where_it_started() {
        let tmp = searchable();

        let mut nav = open(tmp.path(), Some("other.txt"));
        press(&mut nav, '/');
        type_name(&mut nav, "m1");
        assert_eq!(selected(&nav), "m1.txt");

        // "m1x" matches nothing, and the cursor must not be left on the m1.txt the
        // query no longer describes.
        type_name(&mut nav, "x");
        assert_eq!(selected(&nav), "other.txt");

        // Backspacing that letter finds it again.
        key(&mut nav, KeyCode::Backspace);
        assert_eq!(selected(&nav), "m1.txt");
    }

    #[test]
    fn n_walks_the_matches_and_wraps() {
        let tmp = searchable();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '/');
        type_name(&mut nav, "m");
        key(&mut nav, KeyCode::Enter);
        assert_eq!(selected(&nav), "m1.txt");

        press(&mut nav, 'n');
        assert_eq!(selected(&nav), "m2.txt");

        press(&mut nav, 'n');
        assert_eq!(selected(&nav), "m1.txt", "past the last match it wraps");

        press(&mut nav, 'N');
        assert_eq!(selected(&nav), "m2.txt", "and back the other way");
    }

    #[test]
    fn n_does_nothing_until_something_has_been_searched_for() {
        let tmp = searchable();

        let mut nav = open(tmp.path(), Some("other.txt"));
        press(&mut nav, 'n');
        press(&mut nav, 'N');

        assert_eq!(selected(&nav), "other.txt");
    }

    #[test]
    fn a_lowercase_search_ignores_case() {
        let tmp = TempDir::new();
        tmp.file("Readme.md", "");
        tmp.file("notes.txt", "");

        let mut nav = open(tmp.path(), Some("notes.txt"));
        press(&mut nav, '/');
        type_name(&mut nav, "read");

        assert_eq!(selected(&nav), "Readme.md");
    }

    #[test]
    fn a_capital_in_the_search_makes_it_case_sensitive() {
        let tmp = TempDir::new();
        tmp.file("Readme.md", "");
        tmp.file("notes.txt", "");

        let mut nav = open(tmp.path(), Some("notes.txt"));
        press(&mut nav, '/');
        type_name(&mut nav, "READ");

        assert_eq!(selected(&nav), "notes.txt", "READ should match nothing");
    }

    #[test]
    fn the_search_lights_up_every_match_in_the_listing() {
        let tmp = searchable();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '/');
        type_name(&mut nav, "m");

        // Both matching rows, not only the one the cursor landed on.
        assert_eq!(lit(&mut nav, 80, 24), ["m", "m"]);
    }

    #[test]
    fn a_name_matching_twice_is_lit_twice() {
        let tmp = TempDir::new();
        tmp.file("banana.txt", "");

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '/');
        type_name(&mut nav, "an");

        assert_eq!(lit(&mut nav, 80, 24), ["anan"]);
    }

    #[test]
    fn the_highlight_lands_on_the_match_in_a_name_that_is_not_ascii() {
        let tmp = TempDir::new();
        tmp.file("Ärger.txt", "");

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '/');
        type_name(&mut nav, "är");

        // Lowering "Ä" could have moved every later byte; the highlight is placed
        // by where the match falls in the name, not in the lowered copy.
        assert_eq!(lit(&mut nav, 80, 24), ["Är"]);
    }

    #[test]
    fn a_row_the_search_jumped_to_is_always_lit() {
        let tmp = TempDir::new();
        // "İ" is the one letter that lowercases into two characters, so a match on
        // the "i" it becomes covers only part of what is on the screen.
        tmp.file("İstanbul.txt", "");
        tmp.file("other.txt", "");

        let mut nav = open(tmp.path(), Some("other.txt"));
        press(&mut nav, '/');
        type_name(&mut nav, "i");

        assert_eq!(selected(&nav), "İstanbul.txt");
        assert_eq!(lit(&mut nav, 80, 24), ["İ"]);
    }

    #[test]
    fn the_highlight_outlives_the_prompt_and_goes_out_on_escape() {
        let tmp = searchable();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '/');
        type_name(&mut nav, "m1");
        key(&mut nav, KeyCode::Enter);
        assert_eq!(lit(&mut nav, 80, 24), ["m1"], "still lit after enter");

        key(&mut nav, KeyCode::Esc);
        assert!(lit(&mut nav, 80, 24).is_empty());
    }

    #[test]
    fn escape_puts_out_the_highlight_before_it_touches_the_marks() {
        let tmp = searchable();

        let mut nav = open(tmp.path(), None);
        mark(&mut nav, "other.txt");
        press(&mut nav, '/');
        type_name(&mut nav, "m1");
        key(&mut nav, KeyCode::Enter);

        key(&mut nav, KeyCode::Esc);
        assert!(lit(&mut nav, 80, 24).is_empty());
        assert_eq!(nav.marks.len(), 1, "the marks should have survived");

        key(&mut nav, KeyCode::Esc);
        assert!(nav.marks.is_empty());
    }

    #[test]
    fn an_abandoned_search_leaves_nothing_lit() {
        let tmp = searchable();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '/');
        type_name(&mut nav, "m1");
        key(&mut nav, KeyCode::Esc);

        assert!(lit(&mut nav, 80, 24).is_empty());
    }

    #[test]
    fn a_search_holding_a_slash_is_not_refused_as_a_name() {
        let tmp = searchable();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '/');
        type_name(&mut nav, "src/m1");
        key(&mut nav, KeyCode::Enter);

        assert!(
            matches!(nav.mode, Mode::Normal),
            "a query is not a file name, so the prompt should close"
        );
    }

    #[test]
    fn searching_stages_nothing() {
        let tmp = searchable();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, '/');
        // Every one of these is a staging key in normal mode.
        type_name(&mut nav, "cmdaAr");
        key(&mut nav, KeyCode::Esc);

        assert!(nav.plan.is_empty());
    }

    #[test]
    fn z_asks_where_to_jump_to() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, 'z');

        let Mode::Prompt(prompt) = &nav.mode else {
            panic!("z should open a prompt");
        };
        assert_eq!(prompt.label, "jump to");
    }

    #[test]
    fn an_empty_jump_goes_nowhere() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        let before = nav.current_dir.clone();
        press(&mut nav, 'z');
        key(&mut nav, KeyCode::Enter);

        assert!(matches!(nav.mode, Mode::Normal));
        assert_eq!(nav.current_dir, before);
    }

    #[test]
    fn a_jump_zoxide_cannot_answer_leaves_you_where_you_are() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        let before = nav.current_dir.clone();
        press(&mut nav, 'z');
        // A query is a search, not a name: the slash must reach zoxide instead of
        // being refused as unusable. Nothing is anywhere near this one.
        type_name(&mut nav, "nav/zoxide/no-such-place-4f3c");
        key(&mut nav, KeyCode::Enter);

        assert!(matches!(nav.mode, Mode::Normal), "the prompt should close");
        assert_eq!(nav.current_dir, before);
    }

    #[test]
    fn typing_a_name_does_not_trigger_normal_keys() {
        let tmp = TempDir::new();
        tmp.file("victim.txt", "");

        let mut nav = open(tmp.path(), Some("victim.txt"));
        press(&mut nav, 'a');
        // Every one of these is a staging key in normal mode.
        type_name(&mut nav, "dcmqp");
        key(&mut nav, KeyCode::Enter);

        assert_eq!(nav.plan.len(), 1);
        assert_eq!(nav.plan.ops().next().unwrap().describe().subject, "dcmqp");
    }

    #[test]
    fn review_only_opens_when_something_is_staged() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, 'p');

        assert!(matches!(nav.mode, Mode::Normal));
    }

    #[test]
    fn a_source_that_vanished_after_staging_is_blocked_not_attempted() {
        let tmp = TempDir::new();
        let vanishing = tmp.file("vanishing.txt", "");

        let mut nav = open(tmp.path(), Some("vanishing.txt"));
        press(&mut nav, 'd');
        // Deleted behind the navigator's back, after it was staged.
        std::fs::remove_file(&vanishing).unwrap();
        apply(&mut nav);

        assert_eq!(nav.plan.len(), 1, "preflight should refuse the whole plan");
        assert!(nav.plan.iter().next().unwrap().failure.is_none());
    }

    /// Stages a move into a directory, then deletes that directory. Preflight sees
    /// a live source and a free destination, so this only fails once it runs.
    fn stage_a_move_that_will_fail(tmp: &TempDir) -> Navigator {
        tmp.file("mover.txt", "");
        tmp.dir("dest");

        let mut nav = open(tmp.path(), Some("mover.txt"));
        press(&mut nav, ' ');
        nav.go_to(tmp.path().join("dest")).unwrap();
        press(&mut nav, 'm');
        nav.go_to_parent_directory().unwrap();
        key(&mut nav, KeyCode::Esc);

        std::fs::remove_dir(tmp.path().join("dest")).unwrap();
        nav
    }

    #[test]
    fn a_failed_operation_stays_staged_with_its_reason() {
        let tmp = TempDir::new();
        let mut nav = stage_a_move_that_will_fail(&tmp);

        apply(&mut nav);

        assert_eq!(nav.plan.len(), 1);
        assert!(
            nav.plan.iter().next().unwrap().failure.is_some(),
            "the reason it failed should be kept for a retry"
        );
    }

    #[test]
    fn one_failure_does_not_stop_the_operations_after_it() {
        let tmp = TempDir::new();
        tmp.file("survivor.txt", "");
        let mut nav = stage_a_move_that_will_fail(&tmp);

        // Staged after the doomed move, so it only runs if the failure is survived.
        nav.selected = index_of(&nav.entries, "survivor.txt").unwrap();
        press(&mut nav, 'd');
        assert_eq!(nav.plan.len(), 2);

        apply(&mut nav);

        assert_eq!(nav.plan.len(), 1, "only the failure should remain");
        assert!(
            !tmp.path().join("survivor.txt").exists(),
            "the operation after the failure must still have run"
        );
    }

    #[test]
    fn two_entries_of_one_name_cannot_both_land_on_the_same_destination() {
        let tmp = TempDir::new();
        tmp.file("one/notes.txt", "from one");
        tmp.file("two/notes.txt", "from two");
        tmp.dir("dest");

        // Both marks pass preflight: neither destination exists until the other
        // operation has run.
        let mut nav = open(&tmp.path().join("one"), Some("notes.txt"));
        press(&mut nav, ' ');
        nav.go_to(tmp.path().join("two")).unwrap();
        mark(&mut nav, "notes.txt");
        nav.go_to(tmp.path().join("dest")).unwrap();
        press(&mut nav, 'c');
        apply(&mut nav);

        assert_eq!(read(&tmp.path().join("dest/notes.txt")), "from one");
        assert_eq!(
            nav.plan.len(),
            1,
            "the one that could not have its name must stay staged"
        );
        assert!(nav.plan.iter().next().unwrap().failure.is_some());
    }

    #[test]
    fn renaming_in_review_cannot_point_a_trash_at_another_file() {
        let tmp = TempDir::new();
        tmp.file("doomed.txt", "delete me");
        tmp.file("keeper.txt", "keep me");

        let mut nav = open(tmp.path(), Some("doomed.txt"));
        press(&mut nav, 'd');
        press(&mut nav, 'p');
        press(&mut nav, 'r');

        assert!(
            matches!(nav.mode, Mode::Review),
            "there is no destination to rename, so no prompt should open"
        );

        key(&mut nav, KeyCode::Enter);
        assert!(!tmp.path().join("doomed.txt").exists());
        assert_eq!(read(&tmp.path().join("keeper.txt")), "keep me");
    }

    #[test]
    fn the_review_cursor_stays_on_screen_when_the_plan_is_long() {
        let tmp = TempDir::new();
        for i in 0..30 {
            tmp.file(&format!("file{i:02}.txt"), "");
        }

        let mut nav = open(tmp.path(), None);
        for i in 0..30 {
            mark(&mut nav, &format!("file{i:02}.txt"));
        }
        press(&mut nav, 'd');
        press(&mut nav, 'p');
        for _ in 0..29 {
            press(&mut nav, 'j');
        }
        let out = render_to_string(&mut nav, 60, 12);

        // `x` drops the row the cursor is on, so it has to be a row you can see.
        assert!(out.contains("file29.txt"), "in:\n{out}");
        assert!(
            out.lines().any(|line| line.starts_with(CURSOR_BAR)),
            "the cursor scrolled off the screen:\n{out}"
        );
    }

    #[test]
    fn a_prompt_is_visible_with_the_log_panel_open() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");

        let mut nav = open(tmp.path(), Some("a.txt"));
        press(&mut nav, 'L');
        press(&mut nav, 'a');
        type_name(&mut nav, "typed.txt");
        let out = render_to_string(&mut nav, 80, 24);

        assert!(out.contains("new file"), "the label is missing in:\n{out}");
        assert!(
            out.contains("typed.txt"),
            "what was typed is hidden in:\n{out}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_read_does_not_end_the_session() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new();
        let locked = tmp.dir("locked");
        tmp.file("marked.txt", "");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let mut nav = open(tmp.path(), Some("marked.txt"));
        press(&mut nav, ' ');
        nav.selected = index_of(&nav.entries, "locked").unwrap();
        let outcome = nav.handle_key(KeyEvent::from(KeyCode::Enter));

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            outcome,
            Outcome::Stay,
            "a directory you cannot read is not a reason to quit"
        );
        assert_eq!(nav.current_dir, tmp.path(), "and it stays where it was");
        assert_eq!(nav.marks.len(), 1, "with the session's work intact");
    }

    #[test]
    fn trashing_the_directory_you_are_standing_in_backs_out_of_it() {
        let tmp = TempDir::new();
        tmp.file("doomed/inside.txt", "");

        let mut nav = open(tmp.path(), Some("doomed"));
        press(&mut nav, ' ');
        nav.enter_selected().unwrap();
        press(&mut nav, 'd');
        apply(&mut nav);

        assert!(!tmp.path().join("doomed").exists());
        assert_eq!(
            nav.current_dir,
            tmp.path(),
            "standing in a directory that is gone shows a listing that is a lie"
        );
    }

    #[test]
    fn the_plan_panel_shows_staged_work() {
        let tmp = TempDir::new();
        tmp.file("doomed.txt", "");

        let mut nav = open(tmp.path(), Some("doomed.txt"));
        press(&mut nav, 'd');
        let out = render_to_string(&mut nav, 80, 24);

        assert!(out.contains("plan (1)"), "in:\n{out}");
        assert!(out.contains("trash"), "in:\n{out}");
    }

    #[test]
    fn the_review_screen_names_what_is_blocked() {
        let tmp = TempDir::new();
        tmp.file("clash.txt", "");
        tmp.file("dest/clash.txt", "");

        let mut nav = open(tmp.path(), Some("clash.txt"));
        press(&mut nav, ' ');
        nav.go_to(tmp.path().join("dest")).unwrap();
        press(&mut nav, 'c');
        press(&mut nav, 'p');
        let out = render_to_string(&mut nav, 100, 24);

        assert!(out.contains("review"), "in:\n{out}");
        assert!(out.contains("1 blocked"), "in:\n{out}");
        assert!(out.contains("destination already exists"), "in:\n{out}");
    }

    #[test]
    fn marks_are_shown_in_the_listing_and_counted_in_the_status_line() {
        let tmp = TempDir::new();
        tmp.file("marked.txt", "");
        tmp.dir("elsewhere");

        let mut nav = open(tmp.path(), Some("marked.txt"));
        press(&mut nav, ' ');
        nav.go_to(tmp.path().join("elsewhere")).unwrap();
        let out = render_to_string(&mut nav, 80, 24);

        assert!(out.contains("1 marked (1 elsewhere)"), "in:\n{out}");
    }

    #[test]
    fn moving_stops_at_both_ends_of_the_list() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");
        tmp.file("b.txt", "");

        let mut nav = open(tmp.path(), None);

        nav.move_down_by(999);
        assert_eq!(selected(&nav), "b.txt");

        nav.move_up_by(999);
        assert_eq!(selected(&nav), ".");
    }

    #[test]
    fn the_end_of_a_listing_fills_the_screen() {
        let tmp = TempDir::new();
        for i in 0..20 {
            tmp.file(&format!("file{i:02}.txt"), "");
        }

        let mut nav = open(tmp.path(), Some("file19.txt"));
        let out = render_to_string(&mut nav, 40, 12);

        // The scrolloff would otherwise scroll eight rows past the last entry,
        // leaving most of the screen blank while entries sit above the top.
        assert!(out.contains("file19.txt"), "in:\n{out}");
        assert!(
            out.contains("file11.txt"),
            "half the screen is blank in:\n{out}"
        );
    }

    #[test]
    fn a_letter_held_with_ctrl_is_not_the_letter() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");

        let mut nav = open(tmp.path(), Some("a.txt"));
        for c in ['c', 'm', 'd', 'a', 'r', 'y'] {
            nav.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        }

        assert!(nav.plan.is_empty(), "ctrl-c must not stage a copy");
        assert!(matches!(nav.mode, Mode::Normal), "nor open a prompt");
    }

    #[test]
    fn ctrl_and_a_letter_does_not_type_the_letter() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        press(&mut nav, 'a');
        type_name(&mut nav, "notes");
        nav.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_name(&mut nav, ".txt");
        key(&mut nav, KeyCode::Enter);

        assert_eq!(
            nav.plan.ops().next().unwrap().describe().subject,
            "notes.txt"
        );
    }

    #[test]
    fn escape_from_a_rename_goes_back_to_the_review() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");
        tmp.dir("dest");

        let mut nav = open(tmp.path(), Some("a.txt"));
        press(&mut nav, ' ');
        nav.go_to(tmp.path().join("dest")).unwrap();
        press(&mut nav, 'c');
        press(&mut nav, 'p');
        press(&mut nav, 'r');
        key(&mut nav, KeyCode::Esc);

        assert!(
            matches!(nav.mode, Mode::Review),
            "cancelling a rename should not also leave the review"
        );
        assert_eq!(nav.plan.len(), 1);
    }

    #[test]
    fn navigating_away_drops_the_cached_preview() {
        let tmp = TempDir::new();
        tmp.dir("sub");
        tmp.file("a.txt", "");

        let mut nav = open(tmp.path(), Some("sub"));
        nav.cached_image = None;
        nav.enter_selected().unwrap();

        assert!(nav.cached_image.is_none());
        assert_eq!(nav.scroll_offset, 0);
    }

    #[test]
    fn every_screen_survives_a_terminal_of_any_size() {
        let tmp = TempDir::new();
        for i in 0..20 {
            tmp.file(&format!("file{i:02}.txt"), "");
        }

        // A floating terminal is whatever size the window is, and drawing a row
        // that is not there is a panic rather than a smaller screen.
        for (width, height) in [(1, 1), (2, 2), (4, 3), (12, 5), (200, 1), (3, 40), (80, 24)] {
            let mut nav = open(tmp.path(), Some("file19.txt"));
            mark(&mut nav, "file01.txt");
            press(&mut nav, 'd');
            render_to_string(&mut nav, width, height);

            press(&mut nav, '/');
            type_name(&mut nav, "file1");
            render_to_string(&mut nav, width, height);
            key(&mut nav, KeyCode::Esc);

            press(&mut nav, 'L');
            render_to_string(&mut nav, width, height);
            press(&mut nav, 'L');

            press(&mut nav, '?');
            render_to_string(&mut nav, width, height);
            press(&mut nav, '?');

            press(&mut nav, 'p');
            render_to_string(&mut nav, width, height);
        }
    }

    #[test]
    fn an_empty_directory_still_lists_the_dots() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        let out = render_to_string(&mut nav, 40, 10);

        assert_eq!(nav.entries.len(), 2);
        assert!(out.contains('.'), "missing dot entries in:\n{out}");
    }

    #[test]
    fn a_directory_preview_lists_its_contents() {
        let tmp = TempDir::new();
        tmp.file("sub/nested.txt", "");

        let mut nav = open(tmp.path(), Some("sub"));
        let out = render_to_string(&mut nav, 80, 24);

        assert!(out.contains("nested.txt"), "missing preview in:\n{out}");
    }

    #[test]
    fn a_binary_file_preview_says_so() {
        let tmp = TempDir::new();
        tmp.file("app.bin", [0x00, 0x01, 0x02, 0x03]);

        let mut nav = open(tmp.path(), Some("app.bin"));
        let out = render_to_string(&mut nav, 80, 24);

        assert!(out.contains("(binary file)"), "in:\n{out}");
    }

    #[test]
    fn a_broken_image_preview_says_so() {
        let tmp = TempDir::new();
        tmp.file("broken.png", "not actually a png");

        let mut nav = open(tmp.path(), Some("broken.png"));
        let out = render_to_string(&mut nav, 80, 24);

        assert!(out.contains("(cannot load image)"), "in:\n{out}");
    }
}
