#![windows_subsystem = "windows"]

mod elevation;
mod lcu;
mod update;

use lcu::{Reply, Request, Snapshot};
use native_windows_gui as nwg;
use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc, sync::mpsc};

#[derive(Default, serde::Deserialize, serde::Serialize)]
struct Cache {
    latest: Option<String>,
    accounts: HashMap<String, Snapshot>,
    details: HashMap<String, String>,
}

fn cache_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("LolRecords/cache.json"))
}

impl Cache {
    fn load() -> Self {
        cache_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let path = cache_path().ok_or("找不到本地数据目录")?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    fn current(&self) -> Option<&Snapshot> {
        self.accounts.get(self.latest.as_ref()?)
    }

    fn merge(&mut self, mut snapshot: Snapshot) {
        if let Some(previous) = self.accounts.get(&snapshot.account) {
            let mut seen: std::collections::HashSet<_> =
                snapshot.matches.iter().map(|game| game.id).collect();
            snapshot.matches.extend(
                previous
                    .matches
                    .iter()
                    .filter(|game| seen.insert(game.id))
                    .cloned(),
            );
        }
        snapshot
            .matches
            .sort_by_key(|game| std::cmp::Reverse((game.timestamp, game.id)));
        self.latest = Some(snapshot.account.clone());
        self.accounts.insert(snapshot.account.clone(), snapshot);
    }
}

enum UpdateEvent {
    Available(update::Release),
    Finished(Result<(), &'static str>),
}

struct App {
    window: nwg::Window,
    sync: nwg::Button,
    details_button: nwg::Button,
    update_button: nwg::Button,
    update_notice: nwg::Notice,
    update_tx: mpsc::Sender<UpdateEvent>,
    update_rx: mpsc::Receiver<UpdateEvent>,
    release: RefCell<Option<update::Release>>,
    status: nwg::Label,
    account: nwg::Label,
    ranks: nwg::Label,
    matches_heading: nwg::Label,
    detail_heading: nwg::Label,
    account_font: nwg::Font,
    section_font: nwg::Font,
    list: nwg::ListView,
    detail: nwg::TextBox,
    notice: nwg::Notice,
    cache: RefCell<Cache>,
    requests: mpsc::Sender<(Request, Option<String>)>,
    rx: mpsc::Receiver<(Option<String>, Reply)>,
}

fn time(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp_millis(timestamp)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%m-%d %H:%M")
                .to_string()
        })
        .filter(|_| timestamp > 0)
        .unwrap_or_else(|| "—".into())
}

impl App {
    fn update_event(&self) {
        match self.update_rx.try_recv() {
            Ok(UpdateEvent::Available(release)) => {
                self.update_button
                    .set_text(&format!("更新到 {}", release.tag));
                self.update_button.set_visible(true);
                *self.release.borrow_mut() = Some(release);
            }
            Ok(UpdateEvent::Finished(Ok(()))) => nwg::stop_thread_dispatch(),
            Ok(UpdateEvent::Finished(Err(error))) => {
                self.update_button.set_enabled(true);
                self.sync.set_enabled(true);
                self.details_button.set_enabled(true);
                self.status.set_text(error);
            }
            Err(_) => (),
        }
    }

    fn update(&self) {
        if !self.sync.enabled() {
            self.status.set_text("请等待当前战绩查询完成。");
            return;
        }
        let Some(release) = self.release.borrow().clone() else {
            return;
        };
        self.update_button.set_enabled(false);
        self.sync.set_enabled(false);
        self.details_button.set_enabled(false);
        self.status.set_text("正在下载并验证更新…");
        let (tx, notify) = (self.update_tx.clone(), self.update_notice.sender());
        std::thread::spawn(move || {
            let _ = tx.send(UpdateEvent::Finished(update::stage(&release)));
            notify.notice();
        });
    }

    fn layout(&self) {
        let (width, height) = self.window.size();
        let content = width.saturating_sub(48).max(1);
        let action_left = width.saturating_sub(158) as i32;
        self.sync.set_position(action_left, 24);
        self.update_button.set_position(action_left - 134, 24);
        self.account
            .set_size(content.saturating_sub(270).max(1), 34);
        self.ranks.set_size(content, 24);
        self.status.set_size(content, 24);
        let list_height = (height.saturating_sub(218) * 3 / 5).max(66);
        self.list.set_size(content, list_height);
        let detail_top = 164 + list_height;
        self.detail_heading.set_position(24, detail_top as i32 + 7);
        self.details_button
            .set_position(width.saturating_sub(126) as i32, detail_top as i32);
        self.detail.set_position(24, detail_top as i32 + 42);
        self.detail
            .set_size(content, height.saturating_sub(detail_top + 64).max(1));
        for (index, share) in [20, 19, 15, 15, 13, 18].into_iter().enumerate() {
            self.list
                .set_column_width(index, (content.saturating_sub(20) * share / 100) as isize);
        }
    }

    fn render(&self) {
        let cache = self.cache.borrow();
        self.list.clear();
        if let Some(snapshot) = cache.current() {
            self.account.set_text(&snapshot.name);
            self.ranks.set_text(&format!(
                "等级 {}     {}",
                snapshot.level,
                snapshot.ranks.join("     ")
            ));
            self.status.set_text(&format!(
                "{} 场已缓存  ·  上次同步 {}",
                snapshot.matches.len(),
                time(snapshot.synced)
            ));
            for game in &snapshot.matches {
                let result = game
                    .stats
                    .as_ref()
                    .map(|stats| if stats.win { "胜" } else { "负" })
                    .unwrap_or("—");
                let kda = game
                    .stats
                    .as_ref()
                    .map(|s| format!("{}/{}/{}", s.kills, s.deaths, s.assists))
                    .unwrap_or_else(|| "—".into());
                self.list.insert_items_row(
                    None,
                    &[
                        time(game.timestamp),
                        game.mode.clone(),
                        game.platform.clone(),
                        if game.champion > 0 {
                            game.champion.to_string()
                        } else {
                            "—".into()
                        },
                        result.into(),
                        kda,
                    ],
                );
            }
        } else {
            self.account.set_text("尚未连接");
            self.ranks.set_text("");
            self.status
                .set_text("请先登录游戏大厅，再点击「连接并同步」。");
        }
    }

    fn request(&self, request: Request, key: Option<String>) {
        self.sync.set_enabled(false);
        self.details_button.set_enabled(false);
        self.status.set_text("正在读取英雄联盟客户端…");
        if self.requests.send((request, key)).is_err() {
            self.sync.set_enabled(true);
            self.details_button.set_enabled(true);
            self.status.set_text("查询线程已退出，请重新打开工具。");
        }
    }

    fn open_detail(&self) {
        if !self.details_button.enabled() {
            return;
        }
        let cache = self.cache.borrow();
        let Some(snapshot) = cache.current() else {
            return;
        };
        let Some(game) = self
            .list
            .selected_item()
            .and_then(|row| snapshot.matches.get(row))
        else {
            self.status.set_text("请选择一场对局。");
            return;
        };
        let key = format!("{}:{}", snapshot.account, game.id);
        if let Some(details) = cache.details.get(&key) {
            self.detail.set_text(details);
        } else {
            self.detail.set_text("");
            self.request(Request::Detail(game.id), Some(key));
        }
    }

    fn finish(&self) {
        let Ok((key, reply)) = self.rx.try_recv() else {
            return;
        };
        self.sync.set_enabled(true);
        self.details_button.set_enabled(true);
        match reply {
            Reply::Snapshot(snapshot) => {
                self.cache.borrow_mut().merge(*snapshot);
                self.detail.set_text("选择一场对局后，点击「查看详情」。");
                self.render();
            }
            Reply::Detail(details) => {
                self.detail.set_text(&details);
                if let Some(key) = key {
                    self.cache.borrow_mut().details.insert(key, details);
                }
                self.status.set_text("已读取单局详情。");
            }
            Reply::AccessDenied => {
                self.status
                    .set_text("管理员同步进程仍无法读取客户端连接信息。");
                return;
            }
            Reply::Error(message) => {
                self.status.set_text(&message);
                return;
            }
        }
        if self.cache.borrow().save().is_err() {
            self.status.set_text("查询成功，但写入本地缓存失败。");
        }
    }
}

fn gui(update_ready: bool) -> Result<(), nwg::NwgError> {
    nwg::init()?;
    nwg::Font::set_global_family("Microsoft YaHei UI")?;
    let (tx, rx) = mpsc::channel();
    let (requests, jobs) = mpsc::channel();
    let (update_tx, update_rx) = mpsc::channel();
    let mut app = App {
        window: Default::default(),
        sync: Default::default(),
        details_button: Default::default(),
        update_button: Default::default(),
        update_notice: Default::default(),
        update_tx,
        update_rx,
        release: RefCell::new(None),
        status: Default::default(),
        account: Default::default(),
        ranks: Default::default(),
        matches_heading: Default::default(),
        detail_heading: Default::default(),
        account_font: Default::default(),
        section_font: Default::default(),
        list: Default::default(),
        detail: Default::default(),
        notice: Default::default(),
        cache: RefCell::new(Cache::load()),
        requests,
        rx,
    };
    nwg::Window::builder()
        .size((850, 690))
        .center(true)
        .title("国服战绩查询")
        .flags(nwg::WindowFlags::MAIN_WINDOW)
        .build(&mut app.window)?;
    nwg::Font::builder()
        .family("Microsoft YaHei UI")
        .size(20)
        .weight(600)
        .build(&mut app.account_font)?;
    nwg::Font::builder()
        .family("Microsoft YaHei UI")
        .size(11)
        .weight(600)
        .build(&mut app.section_font)?;
    nwg::Button::builder()
        .text("连接并同步")
        .size((134, 36))
        .parent(&app.window)
        .build(&mut app.sync)?;
    nwg::Button::builder()
        .text("查看详情")
        .size((102, 30))
        .parent(&app.window)
        .build(&mut app.details_button)?;
    nwg::Label::builder()
        .position((24, 20))
        .font(Some(&app.account_font))
        .parent(&app.window)
        .build(&mut app.account)?;
    nwg::Label::builder()
        .position((25, 62))
        .parent(&app.window)
        .build(&mut app.ranks)?;
    nwg::Label::builder()
        .position((25, 91))
        .parent(&app.window)
        .build(&mut app.status)?;
    nwg::Label::builder()
        .text("近期战绩")
        .position((24, 118))
        .size((160, 25))
        .font(Some(&app.section_font))
        .parent(&app.window)
        .build(&mut app.matches_heading)?;
    nwg::Label::builder()
        .text("对局详情")
        .size((160, 25))
        .font(Some(&app.section_font))
        .parent(&app.window)
        .build(&mut app.detail_heading)?;
    nwg::ListView::builder()
        .size((802, 285))
        .position((24, 147))
        .parent(&app.window)
        .list_style(nwg::ListViewStyle::Detailed)
        .background_color([255, 255, 255])
        .text_color([30, 41, 59])
        .ex_flags(nwg::ListViewExFlags::FULL_ROW_SELECT)
        .build(&mut app.list)?;
    for (label, width) in [
        ("时间", 132),
        ("模式", 169),
        ("大区", 100),
        ("英雄 ID", 110),
        ("结果", 90),
        ("K / D / A", 128),
    ] {
        app.list.insert_column(nwg::InsertListViewColumn {
            text: Some(label.into()),
            width: Some(width),
            ..Default::default()
        });
    }
    nwg::TextBox::builder()
        .text("选择一场对局后，点击「查看详情」。")
        .size((802, 206))
        .position((16, 408))
        .parent(&app.window)
        .readonly(true)
        .flags(
            nwg::TextBoxFlags::VISIBLE
                | nwg::TextBoxFlags::VSCROLL
                | nwg::TextBoxFlags::AUTOVSCROLL,
        )
        .build(&mut app.detail)?;
    nwg::Notice::builder()
        .parent(&app.window)
        .build(&mut app.notice)?;
    nwg::Notice::builder()
        .parent(&app.window)
        .build(&mut app.update_notice)?;
    nwg::Button::builder()
        .text("检查更新")
        .size((122, 36))
        .position((617, 13))
        .parent(&app.window)
        .build(&mut app.update_button)?;
    app.update_button.set_visible(false);
    let app = Rc::new(app);
    app.layout();
    app.render();
    let weak = Rc::downgrade(&app);
    let handler = nwg::full_bind_event_handler(&app.window.handle, move |event, data, handle| {
        use nwg::Event::*;
        let Some(app) = weak.upgrade() else { return };
        match event {
            OnWindowClose if handle == app.window => nwg::stop_thread_dispatch(),
            OnMinMaxInfo if handle == app.window => data.on_min_max().set_min_size(650, 500),
            OnResize | OnWindowMaximize if handle == app.window => app.layout(),
            OnButtonClick if handle == app.sync => app.request(Request::Sync, None),
            OnButtonClick if handle == app.details_button => app.open_detail(),
            OnListViewItemActivated if handle == app.list => app.open_detail(),
            OnNotice if handle == app.notice => app.finish(),
            OnNotice if handle == app.update_notice => app.update_event(),
            OnButtonClick if handle == app.update_button => app.update(),
            _ => (),
        }
    });
    let notice = app.notice.sender();
    std::thread::spawn(move || {
        let mut connection: Option<elevation::Connection> = None;
        for (request, key) in jobs {
            let reply = if let Some(session) = &mut connection {
                match session.query(request) {
                    Ok(reply) => reply,
                    Err(error) => {
                        connection = None;
                        Reply::Error(error.into())
                    }
                }
            } else {
                match lcu::run(request) {
                    Reply::AccessDenied if matches!(request, Request::Sync) => {
                        match elevation::connect() {
                            Ok(mut session) => match session.query(request) {
                                Ok(reply) => {
                                    connection = Some(session);
                                    reply
                                }
                                Err(error) => Reply::Error(error.into()),
                            },
                            Err(error) => Reply::Error(error),
                        }
                    }
                    Reply::AccessDenied => Reply::Error("请先点击「连接并同步」授权。".into()),
                    reply => reply,
                }
            };
            if tx.send((key, reply)).is_err() {
                break;
            }
            notice.notice();
        }
    });
    app.window.set_visible(true);
    if update_ready {
        update::ready();
    } else {
        update::clean_helper();
    }
    let (tx, notify) = (app.update_tx.clone(), app.update_notice.sender());
    std::thread::spawn(move || {
        if let Some(release) = update::check() {
            let _ = tx.send(UpdateEvent::Available(release));
            notify.notice();
        }
    });
    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if let [_, mode, pipe, command] = args.as_slice() {
        if mode == "--install-update" {
            if let Ok(pid) = pipe.parse() {
                update::install(pid, command);
            }
            return;
        }
        if mode == "--lcu-worker" {
            if let Ok(parent_pid) = command.parse() {
                elevation::worker(pipe, parent_pid);
            }
            return;
        }
    }
    if let Err(error) = gui(args.get(1).is_some_and(|arg| arg == "--update-ready")) {
        nwg::simple_message("国服战绩查询", &format!("窗口初始化失败。{error}"));
    }
}
