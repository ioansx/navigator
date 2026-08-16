use std::path::{Path, PathBuf};

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
    error::Resultx,
    globals::{
        ACCENT, CURSOR_BAR, DIM, MARK, MARK_DOT, NF_OCT_FILE_DIRECTORY_FILL, SCROLL_JUMP,
        SCROLL_OFF, file_color, level_color,
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
            ("q", "quit"),
        ],
    },
    HelpSection {
        title: "marking",
        keys: &[("space", "mark or unmark"), ("esc", "clear every mark")],
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
        })
    }

    /// Handles one keypress. Returns `Ok(true)` when the navigator should quit.
    pub fn handle_key(&mut self, key: KeyEvent) -> Resultx<bool> {
        match self.mode {
            Mode::Prompt(_) => {
                self.handle_prompt_key(key);
                Ok(false)
            }
            Mode::Review => self.handle_review_key(key),
            Mode::Normal => self.handle_normal_key(key),
            Mode::Help => {
                // Anything at all dismisses it: nobody should have to guess twice.
                self.mode = Mode::Normal;
                Ok(false)
            }
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> Resultx<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Char('q') => {
                log::info!("Navigator quit");
                return Ok(true);
            }
            KeyCode::Char('d') if ctrl => self.move_down_by(SCROLL_JUMP),
            KeyCode::Char('u') if ctrl => self.move_up_by(SCROLL_JUMP),
            KeyCode::Char('j') | KeyCode::Down => self.move_down(),
            KeyCode::Char('k') | KeyCode::Up => self.move_up(),
            KeyCode::Enter | KeyCode::Char('l') => return self.enter_selected(),
            KeyCode::Char('-' | 'h') => self.go_to_parent_directory()?,
            KeyCode::Char('z') => self.begin_prompt("jump to", PromptAction::Jump),
            KeyCode::Char('L') => self.toggle_log_panel(),

            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Esc => self.clear_marks(),
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
        Ok(false)
    }

    fn handle_review_key(&mut self, key: KeyEvent) -> Resultx<bool> {
        match key.code {
            KeyCode::Char('q') => return Ok(true),
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
        Ok(false)
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        let Mode::Prompt(prompt) = &mut self.mode else {
            return;
        };

        match key.code {
            KeyCode::Char(c) => prompt.input.push(c),
            KeyCode::Backspace => {
                prompt.input.pop();
            }
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => self.confirm_prompt(),
            _ => {}
        }
    }

    /// Returns `Ok(true)` if the navigator should quit (file opened in neovim).
    pub fn enter_selected(&mut self) -> Resultx<bool> {
        if self.entries.is_empty() {
            return Ok(false);
        }

        let entry = &self.entries[self.selected];
        let name = entry.name.clone();

        if !entry.is_dir {
            return nvim::open(&self.current_dir.join(&name));
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
        Ok(false)
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

    fn clear_marks(&mut self) {
        if !self.marks.is_empty() {
            log::info!("Cleared {} marks", self.marks.len());
            self.marks.clear();
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
            self.plan.push(build(from, to));
            staged += 1;
        }
        log::info!(
            "Staged {staged} operation(s) into {}",
            self.current_dir.display()
        );
    }

    fn stage_trash(&mut self) {
        let targets = self.targets();
        for path in &targets {
            self.plan.push(Op::Trash(path.clone()));
        }
        log::info!("Staged {} for the trash", targets.len());
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
        let Some(staged) = self.plan.iter().nth(self.review_selected) else {
            return;
        };

        let current = staged
            .op
            .destination()
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

        // The one prompt whose input is a search rather than a file name: it may
        // hold slashes, and nothing on disk is ever named after it.
        if matches!(prompt.action, PromptAction::Jump) {
            self.jump(prompt.input.trim());
            return;
        }

        let name = prompt.input.trim().to_string();
        if let Some(problem) = validate_name(&name) {
            log::warn!("{name:?} is {problem}");
            self.mode = Mode::Prompt(prompt);
            return;
        }

        match prompt.action {
            PromptAction::CreateFile => self.plan.push(Op::CreateFile(self.current_dir.join(name))),
            PromptAction::CreateDir => self.plan.push(Op::CreateDir(self.current_dir.join(name))),
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
            // Taken care of above: a search must not reach the name check.
            PromptAction::Jump => {}
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
    fn refresh(&mut self) -> Resultx<()> {
        let entries = dir::read_dir_with_dots(&self.current_dir)?;
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

        if self.log_panel_visible {
            // Expanded: file list on top, log panel takes bottom 70%
            let chunks = Layout::vertical([Constraint::Percentage(30), Constraint::Percentage(70)])
                .split(area);

            self.render_file_list(chunks[0], buf);
            self.render_log_panel(chunks[1], buf);
            return;
        }

        // File list + preview, the staged plan when there is one, then one line
        // that is either what you are typing or the latest message.
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

        self.render_with_preview(chunks[0], buf);
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

        let rows = chunks[1];
        for (i, staged) in self.plan.iter().take(rows.height as usize).enumerate() {
            plan_row(staged, i == self.review_selected, rows.width).render(row(rows, i), buf);
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

        // Adjust scroll offset to keep selection visible with scrolloff context
        if self.selected < self.scroll_offset + scrolloff {
            self.scroll_offset = self.selected.saturating_sub(scrolloff);
        } else if self.selected + scrolloff >= self.scroll_offset + visible_height {
            self.scroll_offset = (self.selected + scrolloff + 1).saturating_sub(visible_height);
        }

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

            let line = Line::from(vec![
                Span::styled(
                    if under_cursor { CURSOR_BAR } else { " " },
                    Style::new().fg(ACCENT),
                ),
                Span::styled(if marked { MARK_DOT } else { " " }, Style::new().fg(MARK)),
                Span::raw(" "),
                Span::styled(icon_for(entry), Style::new().fg(color)),
                Span::raw(" "),
                Span::styled(entry.name.clone(), name),
            ]);
            line.render(row(inner, i - self.scroll_offset), buf);
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

/// Packs the sections into columns `height` rows tall.
///
/// A section is never split across two columns: half a group of keys stranded at
/// the bottom of one column reads as a different group than it is.
fn help_columns(height: usize) -> Vec<Vec<Line<'static>>> {
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

    fn press(nav: &mut Navigator, c: char) {
        nav.handle_key(KeyEvent::from(KeyCode::Char(c))).unwrap();
    }

    fn key(nav: &mut Navigator, code: KeyCode) {
        nav.handle_key(KeyEvent::from(code)).unwrap();
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
