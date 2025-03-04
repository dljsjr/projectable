pub mod component;
mod components;

use self::component::{Component, Drawable};
pub use self::components::*;
use crate::{
    config::{Config, Key},
    external_event::{ExternalEvent, RefreshData},
    marks::Marks,
    queue::{AppEvent, Queue, TmuxOpts},
};
use anyhow::{Context, Result};
use crossterm::event::Event;
use duct::{cmd, Expression};
use easy_switch::switch;
use log::{error, info, warn};
use std::env;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
use std::process::Command;
use std::{
    cell::RefCell,
    fs::{self, File},
    path::{Path, PathBuf},
    rc::Rc,
};
use tui::{
    backend::Backend,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, Borders},
    Frame,
};
use tui_logger::{TuiLoggerLevelOutput as LoggerLevel, TuiLoggerWidget as Logger};

/// Event that is sent back up to main.rs
#[derive(Debug)]
pub enum TerminalEvent {
    OpenFile(PathBuf),
    RunCommandThreaded(Expression),
    RunCommand(Expression),
    StopAllCommands,
}

pub struct App {
    tree: Filetree,
    path: PathBuf,
    should_quit: bool,
    queue: Queue,
    pending: PendingPopup,
    input_box: InputBox,
    previewer: PreviewFile,
    text_popup: Popup,
    file_cmd_popup: FileCmdPopup,
    marks_popup: MarksPopup,
    fuzzy_matcher: FuzzyMatcher,
    config: Rc<Config>,
}

impl App {
    pub fn new(
        path: PathBuf,
        cwd: impl AsRef<Path>,
        config: Rc<Config>,
        marks: Rc<RefCell<Marks>>,
    ) -> Result<Self> {
        let queue = Queue::new();
        let mut tree = Filetree::from_dir_with_config(
            &path,
            queue.clone(),
            Rc::clone(&config),
            Rc::clone(&marks),
        )?;
        tree.open_path(cwd)?;
        Ok(App {
            path: path.clone(),
            tree,
            should_quit: false,
            pending: PendingPopup::new(queue.clone(), Rc::clone(&config)),
            input_box: InputBox::new(queue.clone()),
            previewer: PreviewFile::with_config(Rc::clone(&config)),
            text_popup: Popup::new(Rc::clone(&config)),
            config: Rc::clone(&config),
            marks_popup: MarksPopup::new(marks, queue.clone(), Rc::clone(&config), path),
            file_cmd_popup: FileCmdPopup::new(queue.clone(), Rc::clone(&config)),
            fuzzy_matcher: FuzzyMatcher::new_with_config(queue.clone(), Rc::clone(&config)),
            queue,
        })
    }

    /// Returns None if no events should be sent to the terminal
    pub fn update(&mut self) -> Result<Option<TerminalEvent>> {
        while let Some(app_event) = self.queue.pop() {
            // Handle events from queue
            match app_event {
                AppEvent::OpenPopup(operation) => self.pending.operation = operation,
                AppEvent::DeleteFile(path) => {
                    if path.is_file() {
                        fs::remove_file(&path)
                            .context("failed to remove file while resolving event queue")?;
                        info!("deleted file \"{}\"", path.display());
                    } else {
                        fs::remove_dir_all(&path)
                            .context("failed to remove dir while resolving event queue")?;
                        info!("deleted directory \"{}\"", path.display());
                    }
                    self.tree.partial_refresh(&RefreshData::Delete(path))?;
                    if let Some(item) = self.tree.get_selected() {
                        self.previewer.preview_file(item.path())?;
                    }
                }
                AppEvent::OpenFile(path) => {
                    info!("opening file \"{}\"", path.display());
                    return Ok(Some(TerminalEvent::OpenFile(path)));
                }
                AppEvent::OpenInput(op) => self.input_box.operation = op,
                AppEvent::NewFile(path) => {
                    File::create(&path)
                        .context("failed to create file while resolving event queue")?;
                    info!("created file \"{}\"", path.display());
                    self.tree.partial_refresh(&RefreshData::Add(path))?;
                }
                AppEvent::NewDir(path) => {
                    fs::create_dir(&path)
                        .context("failed to create dir while resolving event queue")?;
                    info!("created directory \"{}\"", path.display());
                    self.tree.partial_refresh(&RefreshData::Add(path))?;
                }
                AppEvent::RenameFile(old, new) => {
                    let new = old
                        .parent()
                        .context("file to rename has no parent")?
                        .join(new);
                    cmd!("mv", &old, &new).stderr_capture().run()?;
                    info!("renamed file to {}", new.display());
                    self.tree.rename(old, new)?;
                }
                AppEvent::MoveFile(from, to) => {
                    cmd!("mv", &from, &to).stderr_capture().run()?;
                    self.tree.move_item(from, to)?;
                }
                AppEvent::PreviewFile(path) => self
                    .previewer
                    .preview_file(path)
                    .context("failed to preview while resolving event queue")?,
                AppEvent::TogglePreviewMode => self.previewer.toggle_mode(),
                AppEvent::RunCommand(cmd) => {
                    // Strip !!, and if it exists, run in foreground, not background
                    let (threaded, cmd) = cmd
                        .strip_prefix("!!")
                        .map_or((true, cmd.as_str()), |s| (false, s));

                    #[cfg(not(target_os = "windows"))]
                    let cmd = cmd!(
                        env::var("SHELL").unwrap_or_else(|_| "sh".to_owned()),
                        "-c",
                        cmd
                    );
                    #[cfg(target_os = "windows")]
                    let cmd = cmd!("cmd.exe", "/C", cmd);

                    if threaded {
                        self.text_popup.preset = Preset::RunningCommand;
                        return Ok(Some(TerminalEvent::RunCommandThreaded(
                            cmd.stdout_capture()
                                .stderr_capture()
                                .stderr_to_stdout()
                                .stdin_null()
                                .unchecked(),
                        )));
                    } else {
                        return Ok(Some(TerminalEvent::RunCommand(cmd)));
                    };
                }
                AppEvent::RunCommandWithTmux(cmd, opts) => {
                    if env::var("TMUX").is_err() {
                        error!("not in tmux session");
                        continue;
                    }

                    let cmd_expr = match opts {
                        TmuxOpts::NewWindow => cmd!("command", "tmux", "new-window", cmd),
                        TmuxOpts::VerticalSplit => {
                            cmd!("command", "tmux", "split-window", "-h", cmd)
                        }
                        TmuxOpts::HorizontalSplit => {
                            cmd!("command", "tmux", "split-window", "-v", cmd)
                        }
                        TmuxOpts::FloatingWindow => {
                            cmd!("command", "tmux", "display-popup", "-E", cmd)
                        }
                    };
                    let out = cmd_expr.stderr_to_stdout().unchecked().read()?;
                    if out.is_empty() {
                        continue;
                    }

                    info!("opening tmux window");
                    warn!("{out}");
                }
                AppEvent::SearchFiles(files) => {
                    self.fuzzy_matcher.open_path(
                        files
                            .into_iter()
                            .map(|path| {
                                path.strip_prefix(self.path())
                                    .expect("path should start with root")
                                    .display()
                                    .to_string()
                            })
                            .collect(),
                    );
                }
                AppEvent::SpecialCommand(path) => drop(self.file_cmd_popup.open_for(path)),
                AppEvent::GotoFile(path) => {
                    let path = if path.is_relative() {
                        self.path().join(path)
                    } else {
                        path
                    };
                    self.tree.open_path(path)?;
                }
                AppEvent::Mark(path) => {
                    info!("marked: \"{}\"", path.display());
                    self.marks_popup.add_mark(path);
                }
                AppEvent::OpenFuzzy(items, operation) => self.fuzzy_matcher.start(items, operation),
                AppEvent::FilterFor(items) => self.tree.filter_include(&items)?,
                AppEvent::StopAllCommands => {
                    self.text_popup.preset = Preset::Nothing;
                    return Ok(Some(TerminalEvent::StopAllCommands));
                }
            }
        }

        Ok(None)
    }

    pub fn handle_event(&mut self, ev: &ExternalEvent) -> Result<()> {
        let popup_open = self.pending.visible()
            || self.input_box.visible()
            || self.text_popup.visible()
            || self.file_cmd_popup.visible()
            || self.marks_popup.visible()
            || self.fuzzy_matcher.visible();
        // Do not give the Filetree or previewer focus if there are any popups open
        self.tree.focus(!popup_open);
        self.previewer.focus(!popup_open);

        self.pending.handle_event(ev)?;
        self.input_box.handle_event(ev)?;
        self.fuzzy_matcher.handle_event(ev)?;
        self.tree.handle_event(ev)?;
        self.previewer.handle_event(ev)?;
        self.text_popup.handle_event(ev)?;
        self.file_cmd_popup.handle_event(ev)?;
        self.marks_popup.handle_event(ev)?;

        match ev {
            ExternalEvent::Crossterm(Event::Key(key)) => {
                if popup_open {
                    return Ok(());
                }
                switch! { key;
                    self.config.quit => self.should_quit = true,
                    self.config.help => self.text_popup.preset = Preset::Help,
                    self.config.marks.open => self.marks_popup.open(),
                    Key::esc(), self.config.esc_to_close => self.should_quit = true,
                    self.config.kill_processes => self.queue.add(AppEvent::StopAllCommands),
                };
            }
            ExternalEvent::CommandOutput(out) => {
                self.text_popup.preset = Preset::Nothing;
                info!("output:");
                info!("{}", if out.is_empty() { " " } else { out });
            }
            _ => (),
        }
        Ok(())
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drawable for App {
    fn draw<B: Backend>(&self, f: &mut Frame<B>, area: Rect) -> Result<()> {
        let main_layout = Layout::default()
            .direction(Direction::Horizontal)
            .horizontal_margin(1)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(area);
        let left_hand_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)].as_ref())
            .split(main_layout[0]);

        let logger = Logger::default()
            .style_error(self.config.log.error.into())
            .style_debug(self.config.log.debug.into())
            .style_warn(self.config.log.warn.into())
            .style_trace(self.config.log.trace.into())
            .style_info(self.config.log.info.into())
            .output_level(Some(LoggerLevel::Long))
            .output_target(false)
            .output_file(false)
            .output_line(false)
            .output_level(None)
            .output_timestamp(None)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Log")
                    .border_style(self.config.log.border_color.into()),
            );

        self.tree.draw(f, left_hand_layout[0])?;
        f.render_widget(logger, left_hand_layout[1]);
        self.previewer.draw(f, main_layout[1])?;
        self.pending.draw(f, area)?;
        self.input_box.draw(f, area)?;
        self.text_popup.draw(f, area)?;
        self.file_cmd_popup.draw(f, area)?;
        self.marks_popup.draw(f, area)?;
        self.fuzzy_matcher.draw(f, area)?;

        Ok(())
    }
}
