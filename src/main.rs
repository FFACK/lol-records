#![windows_subsystem = "windows"]

mod assets;
mod elevation;
mod lcu;
mod ui;
mod update;

use assets::ImageKey;
use lcu::{Detail, Reply, Request, Snapshot};
use native_windows_gui as nwg;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::mpsc,
};
use ui::{
    Body, DetailLayout, DetailState,
    paint::{self, Pixels, Renderer, Surface},
};
use windows_sys::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect,
        PAINTSTRUCT, SetBkColor, SetTextColor,
    },
    UI::{
        HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
            SetProcessDpiAwarenessContext,
        },
        Input::KeyboardAndMouse::{SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
        WindowsAndMessaging::*,
    },
};

#[derive(Default, serde::Deserialize, serde::Serialize)]
struct Cache {
    latest: Option<String>,
    accounts: HashMap<String, Snapshot>,
    details: HashMap<String, String>,
    #[serde(default)]
    structured_details: HashMap<String, Detail>,
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

    fn put_detail(&mut self, account: &str, id: u64, mut detail: Detail) -> bool {
        if detail.game_id != id {
            return false;
        }
        if let Some((platform, puuid)) = account.split_once(':') {
            if !platform.is_empty()
                && detail
                    .platform
                    .as_deref()
                    .is_some_and(|value| value != platform)
            {
                return false;
            }
            if detail.account_puuid.as_deref() != Some(puuid) {
                detail.self_id = detail
                    .participants
                    .iter()
                    .find(|p| p.puuid.as_deref() == Some(puuid))
                    .and_then(|p| p.participant_id);
            }
        }
        detail.account_puuid = None;
        self.structured_details
            .insert(format!("{account}:{id}"), detail);
        true
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct MatchKey {
    account: String,
    id: u64,
}

impl MatchKey {
    fn cache_key(&self) -> String {
        format!("{}:{}", self.account, self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Ticket {
    key: MatchKey,
    generation: u64,
}

#[derive(Default)]
struct Selection {
    current: Option<Ticket>,
    working: Option<Ticket>,
    generation: u64,
    want_query: bool,
    error: Option<String>,
    detail: DetailState,
}

impl Selection {
    fn select(&mut self, key: Option<MatchKey>, query: bool) {
        if self.current.as_ref().map(|ticket| &ticket.key) == key.as_ref() {
            self.want_query |= query
                && !self
                    .working
                    .as_ref()
                    .is_some_and(|ticket| Some(&ticket.key) == key.as_ref());
            if query && self.error.is_some() {
                self.error = None;
            }
            return;
        }
        self.generation += 1;
        self.current = key.map(|key| Ticket {
            key,
            generation: self.generation,
        });
        self.want_query = query
            && !self.working.as_ref().is_some_and(|working| {
                self.current
                    .as_ref()
                    .is_some_and(|current| current.key == working.key)
            });
        self.error = None;
        self.detail.reset();
    }

    fn next(&mut self) -> Option<Ticket> {
        if !self.want_query || self.working.is_some() {
            return None;
        }
        let ticket = self.current.clone()?;
        self.want_query = false;
        self.working = Some(ticket.clone());
        Some(ticket)
    }

    fn complete(&mut self, ticket: &Ticket) -> bool {
        if self.working.as_ref() == Some(ticket) {
            self.working = None;
        }
        self.current
            .as_ref()
            .is_some_and(|current| current.key == ticket.key)
    }
}

struct App {
    window: nwg::Window,
    sync: nwg::Button,
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
    account_font: RefCell<nwg::Font>,
    section_font: RefCell<nwg::Font>,
    list: nwg::ListBox<String>,
    rows: RefCell<Vec<u64>>,
    dpi: Cell<u32>,
    body_font: RefCell<nwg::Font>,
    detail_frame: nwg::Frame,
    detail_text: nwg::TextBox,
    detail_layout: RefCell<DetailLayout>,
    selection: RefCell<Selection>,
    sync_busy: Cell<bool>,
    update_busy: Cell<bool>,
    hover: Cell<Option<usize>>,
    renderer: Option<Renderer>,
    list_surface: RefCell<Option<Surface>>,
    detail_surface: RefCell<Option<Surface>>,
    list_fallback: Cell<bool>,
    detail_fallback: Cell<bool>,
    detail_repainted: Cell<bool>,
    list_repainted: Cell<bool>,
    header_brush: *mut core::ffi::c_void,
    image_notice: nwg::Notice,
    cache_progress: nwg::Label,
    cache_progress_rx: mpsc::Receiver<String>,
    pixels: RefCell<HashMap<ImageKey, Pixels>>,
    image_requested: RefCell<HashSet<ImageKey>>,
    image_jobs: mpsc::Sender<ImageKey>,
    image_results: mpsc::Receiver<(ImageKey, Option<Vec<u8>>)>,
    notice: nwg::Notice,
    cache: RefCell<Cache>,
    requests: mpsc::Sender<(Request, Option<Ticket>)>,
    rx: mpsc::Receiver<(Option<Ticket>, Reply)>,
}

impl Drop for App {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.header_brush);
        }
    }
}

fn time(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp_millis(timestamp)
        .filter(|_| timestamp > 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "—".into())
}

fn hwnd(handle: nwg::ControlHandle) -> HWND {
    handle.hwnd().expect("created window").cast()
}

fn invalidate(handle: HWND) {
    unsafe {
        InvalidateRect(handle, std::ptr::null(), 0);
    }
}

fn client(handle: HWND) -> (u32, u32, u32) {
    let mut area = unsafe { std::mem::zeroed::<RECT>() };
    unsafe {
        GetClientRect(handle, &mut area);
    }
    let dpi = unsafe { GetDpiForWindow(handle) };
    (
        area.right.max(0) as u32,
        area.bottom.max(0) as u32,
        if dpi == 0 { 96 } else { dpi },
    )
}

fn dip(px: i32, dpi: u32) -> f32 {
    px as f32 * 96.0 / dpi as f32
}
fn physical(value: i32, dpi: u32) -> i32 {
    (value as f32 * dpi as f32 / 96.0).round() as i32
}

fn pane_size(handle: nwg::ControlHandle) -> (f32, f32) {
    let (width, height, dpi) = client(hwnd(handle));
    (dip(width as i32, dpi), dip(height as i32, dpi))
}

fn resize_font(
    target: &RefCell<nwg::Font>,
    size: i32,
    weight: u32,
    dpi: u32,
) -> Option<(nwg::Font, std::cell::Ref<'_, nwg::Font>)> {
    let mut font = nwg::Font::default();
    nwg::Font::builder()
        .family("Microsoft YaHei UI")
        .size_absolute(physical(size, dpi) as u32)
        .weight(weight)
        .build(&mut font)
        .ok()?;
    Some((target.replace(font), target.borrow()))
}

fn mouse(param: isize) -> (i32, i32) {
    (param as i16 as i32, (param >> 16) as i16 as i32)
}
fn leave_tracking(window: HWND) {
    let mut event = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: window,
        dwHoverTime: 0,
    };
    unsafe {
        TrackMouseEvent(&mut event);
    }
}

impl App {
    fn layout(&self) {
        let (width, height, dpi) = client(hwnd(self.window.handle));
        if width == 0 || height == 0 {
            return;
        }
        let width = dip(width as i32, dpi) as i32;
        let height = dip(height as i32, dpi) as i32;
        let panel_height = (height - 144).max(1);
        let detail_width = (width - 383).max(1);
        let place = |control, x: i32, y: i32, w: i32, h: i32| unsafe {
            MoveWindow(
                hwnd(control),
                physical(x, dpi),
                physical(y, dpi),
                physical(w.max(1), dpi),
                physical(h.max(1), dpi),
                1,
            );
        };
        place(self.sync.handle, width - 158, 24, 134, 36);
        place(self.update_button.handle, width - 292, 24, 122, 36);
        place(self.account.handle, 24, 12, width - 315, 34);
        place(self.ranks.handle, 25, 50, width - 50, 24);
        place(self.status.handle, 25, 74, width - 50, 24);
        place(self.matches_heading.handle, 24, 98, 160, 22);
        place(self.detail_heading.handle, 359, 98, 150, 22);
        place(
            self.cache_progress.handle,
            (width - 330).max(515),
            98,
            306,
            22,
        );
        place(self.list.handle, 24, 120, 320, panel_height);
        place(
            self.detail_frame.handle,
            359,
            120,
            detail_width,
            panel_height,
        );
        place(
            self.detail_text.handle,
            359,
            120,
            detail_width,
            panel_height,
        );
        if self.dpi.replace(dpi) != dpi {
            if let Some((_old, font)) = resize_font(&self.account_font, 24, 600, dpi) {
                self.account.set_font(Some(&font));
            }
            if let Some((_old, font)) = resize_font(&self.section_font, 15, 600, dpi) {
                self.matches_heading.set_font(Some(&font));
                self.detail_heading.set_font(Some(&font));
            }
            if let Some((_old, font)) = resize_font(&self.body_font, 14, 400, dpi) {
                self.ranks.set_font(Some(&font));
                self.status.set_font(Some(&font));
                self.cache_progress.set_font(Some(&font));
                self.sync.set_font(Some(&font));
                self.update_button.set_font(Some(&font));
                self.list.set_font(Some(&font));
                self.detail_text.set_font(Some(&font));
            }
        }
        let list = hwnd(self.list.handle);
        let row_height = if self.list_fallback.get() {
            25
        } else {
            ui::ROW_HEIGHT as i32
        };
        unsafe {
            SendMessageW(
                list,
                LB_SETITEMHEIGHT,
                0,
                physical(row_height, dpi) as isize,
            );
        }
        self.refresh_detail();
        invalidate(list);
    }

    fn load_snapshot(&self, keep_scroll: bool) {
        let list = hwnd(self.list.handle);
        let old_top = unsafe { SendMessageW(list, LB_GETTOPINDEX, 0, 0) }.max(0) as usize;
        let anchor = if keep_scroll {
            self.rows.borrow().get(old_top).copied()
        } else {
            None
        };
        let cache = self.cache.borrow();
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
            let index = self
                .selection
                .borrow()
                .current
                .as_ref()
                .filter(|ticket| ticket.key.account == snapshot.account)
                .and_then(|ticket| {
                    snapshot
                        .matches
                        .iter()
                        .position(|game| game.id == ticket.key.id)
                })
                .or_else(|| (!snapshot.matches.is_empty()).then_some(0));
            self.list
                .set_collection(snapshot.matches.iter().map(ui::list_label).collect());
            *self.rows.borrow_mut() = snapshot.matches.iter().map(|game| game.id).collect();
            self.list.set_selection(index);
            if let Some(index) =
                anchor.and_then(|id| snapshot.matches.iter().position(|game| game.id == id))
            {
                unsafe {
                    SendMessageW(list, LB_SETTOPINDEX, index, 0);
                }
            }
            self.selection.borrow_mut().select(
                index.map(|index| MatchKey {
                    account: snapshot.account.clone(),
                    id: snapshot.matches[index].id,
                }),
                false,
            );
        } else {
            self.account.set_text("尚未同步账号");
            self.status.set_text("登录英雄联盟后，点击「连接并同步」。");
        }
        drop(cache);
        self.refresh_detail();
        invalidate(list);
    }

    fn selected<'a>(
        cache: &'a Cache,
        selection: &'a Selection,
    ) -> (Option<&'a lcu::Match>, Body<'a>) {
        let Some(ticket) = &selection.current else {
            return (None, Body::Empty("同步账号后可查看战绩。"));
        };
        let game = cache
            .accounts
            .get(&ticket.key.account)
            .and_then(|snapshot| {
                snapshot
                    .matches
                    .iter()
                    .find(|game| game.id == ticket.key.id)
            });
        let key = ticket.key.cache_key();
        let body = if let Some(detail) = cache.structured_details.get(&key) {
            Body::Structured(detail)
        } else if let Some(legacy) = cache.details.get(&key) {
            Body::Legacy(legacy)
        } else if let Some(error) = &selection.error {
            Body::Empty(error)
        } else if selection.want_query
            || selection
                .working
                .as_ref()
                .is_some_and(|job| job.key == ticket.key)
        {
            Body::Empty("正在读取这场对局的详情…")
        } else {
            Body::Empty("点击战绩行读取详情。游戏未连接时，可浏览已缓存的战绩。")
        };
        (game, body)
    }

    fn refresh_detail(&self) {
        let cache = self.cache.borrow();
        let mut selection = self.selection.borrow_mut();
        let (_, body) = Self::selected(&cache, &selection);
        let legacy = if let Body::Legacy(raw) = &body {
            Some(assets::detail_text(raw))
        } else {
            None
        };
        let body = if let Some(legacy) = &legacy {
            Body::Legacy(legacy)
        } else {
            body
        };
        let (width, height) = pane_size(self.detail_frame.handle);
        let layout = DetailLayout::new(width, &body);
        if self.detail_fallback.get() {
            self.detail_text.set_text(&match body {
                Body::Empty(message) | Body::Legacy(message) => message.to_owned(),
                Body::Structured(detail) => assets::structured_text(detail),
            });
        }
        selection.detail.scroll = selection.detail.scroll.min(layout.limit(height));
        if selection
            .detail
            .item
            .is_some_and(|item| item >= layout.items.len())
        {
            selection.detail.item = None;
        }
        selection.detail.hovered = None;
        *self.detail_layout.borrow_mut() = layout;
        invalidate(hwnd(self.detail_frame.handle));
    }

    fn open_detail(&self) {
        if self.update_busy.get() {
            return;
        }
        let cache = self.cache.borrow();
        let Some(snapshot) = cache.current() else {
            return;
        };
        let Some(game) = self
            .list
            .selection()
            .and_then(|index| snapshot.matches.get(index))
        else {
            return;
        };
        let key = MatchKey {
            account: snapshot.account.clone(),
            id: game.id,
        };
        let cache_key = key.cache_key();
        let query = !cache
            .structured_details
            .get(&cache_key)
            .is_some_and(|detail| {
                detail
                    .participants
                    .iter()
                    .any(|p| p.summoner_name.is_some())
            })
            && (!cache.details.contains_key(&cache_key) || self.sync.text() == "刷新战绩");
        self.selection.borrow_mut().select(Some(key), query);
        drop(cache);
        self.dispatch_detail();
        self.refresh_detail();
        invalidate(hwnd(self.list.handle));
    }

    fn dispatch_detail(&self) {
        if self.sync_busy.get() || self.update_busy.get() {
            return;
        }
        let mut selection = self.selection.borrow_mut();
        let Some(ticket) = selection.next() else {
            return;
        };
        if self
            .cache
            .borrow()
            .structured_details
            .get(&ticket.key.cache_key())
            .is_some_and(|detail| {
                detail
                    .participants
                    .iter()
                    .any(|p| p.summoner_name.is_some())
            })
        {
            selection.complete(&ticket);
        } else if self
            .requests
            .send((Request::Detail(ticket.key.id), Some(ticket.clone())))
            .is_err()
        {
            selection.complete(&ticket);
            selection.error = Some("查询线程已退出，请重新打开工具。".into());
        }
    }

    fn profile_at(&self, x: f32, y: f32) {
        if self.sync_busy.get() || self.update_busy.get() {
            return;
        }
        let index = self
            .detail_layout
            .borrow()
            .players
            .iter()
            .find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
            .map(|(_, index)| *index);
        let puuid = {
            let cache = self.cache.borrow();
            let selection = self.selection.borrow();
            let (_, body) = Self::selected(&cache, &selection);
            match (index, body) {
                (Some(index), Body::Structured(detail)) => {
                    detail.participants.get(index).and_then(|p| p.puuid.clone())
                }
                _ => None,
            }
        };
        let Some(puuid) = puuid.filter(|id| lcu::valid_puuid(id)) else {
            self.status
                .set_text("该玩家标识未缓存，请连接后重新读取这场详情。");
            return;
        };
        self.sync_busy.set(true);
        self.sync.set_enabled(false);
        self.status.set_text("正在读取该召唤师的战绩…");
        if self
            .requests
            .send((Request::Profile(puuid.as_bytes().try_into().unwrap()), None))
            .is_err()
        {
            self.sync_busy.set(false);
            self.sync.set_enabled(true);
        }
    }

    fn sync(&self) {
        self.sync_busy.set(true);
        self.sync.set_enabled(false);
        self.status.set_text("正在同步游戏客户端…");
        if self.requests.send((Request::Sync, None)).is_err() {
            self.sync_busy.set(false);
            self.sync.set_enabled(true);
            self.status.set_text("查询线程已退出，请重新打开工具。");
        }
    }

    fn finish(&self) {
        while let Ok((ticket, reply)) = self.rx.try_recv() {
            let mut saved = false;
            let mut visible = true;
            if let Some(ticket) = ticket {
                let mut selection = self.selection.borrow_mut();
                visible = selection.complete(&ticket);
                let error = match reply {
                    Reply::Detail(detail) => {
                        saved = self.cache.borrow_mut().put_detail(
                            &ticket.key.account,
                            ticket.key.id,
                            *detail,
                        );
                        (!saved).then(|| "该局详情与缓存对局或大区不一致。".into())
                    }
                    Reply::Error(message) => Some(message),
                    _ => Some("客户端未返回该局详情。".into()),
                };
                if visible {
                    selection.error =
                        error.map(|message| format!("{message} 可再次点击该局重试。"));
                    if selection.error.is_some()
                        && self
                            .cache
                            .borrow()
                            .details
                            .contains_key(&ticket.key.cache_key())
                    {
                        self.status
                            .set_text(selection.error.as_deref().unwrap_or_default());
                    }
                }
            } else {
                self.sync_busy.set(false);
                self.sync.set_enabled(true);
                match reply {
                    Reply::Snapshot(snapshot) => {
                        let same = self
                            .cache
                            .borrow()
                            .current()
                            .is_some_and(|old| old.account == snapshot.account);
                        if !same {
                            self.selection.borrow_mut().select(None, false);
                        }
                        self.cache.borrow_mut().merge(*snapshot);
                        self.sync.set_text("刷新战绩");
                        self.load_snapshot(same);
                        self.open_detail();
                        saved = true;
                    }
                    Reply::Error(error) => self.status.set_text(&format!("同步未完成 · {error}")),
                    _ => self.status.set_text("同步未完成，请确认客户端已登录。"),
                }
            }
            if saved && self.cache.borrow().save().is_err() {
                self.status.set_text("读取成功，本次缓存写入失败。");
            }
            self.dispatch_detail();
            if visible {
                self.refresh_detail();
            }
        }
    }

    fn image_arrived(&self) {
        for progress in self.cache_progress_rx.try_iter() {
            self.cache_progress.set_text(&progress);
        }
        let Some(renderer) = &self.renderer else {
            return;
        };
        let mut pixels = self.pixels.borrow_mut();
        let mut changed = false;
        for (key, bytes) in self.image_results.try_iter() {
            if let Some(decoded) = bytes
                .as_deref()
                .and_then(|bytes| renderer.decode(key, bytes))
            {
                pixels.insert(key, decoded);
                changed = true;
            }
        }
        if changed {
            invalidate(hwnd(self.list.handle));
            invalidate(hwnd(self.detail_frame.handle));
        }
    }

    fn paint(&self, child: HWND, left: bool) -> Option<isize> {
        let (fallback, target, retried) = if left {
            (
                &self.list_fallback,
                &self.list_surface,
                &self.list_repainted,
            )
        } else {
            (
                &self.detail_fallback,
                &self.detail_surface,
                &self.detail_repainted,
            )
        };
        if fallback.get() {
            return None;
        }
        let renderer = self.renderer.as_ref()?;
        let (w, h, dpi) = client(child);
        if w == 0 || h == 0 {
            return None;
        }
        let mut target = target.borrow_mut();
        if target
            .as_mut()
            .is_some_and(|surface| surface.resize(w, h, dpi).is_err())
        {
            *target = None;
        }
        if target.is_none() {
            *target = renderer
                .target(windows::Win32::Foundation::HWND(child), w, h, dpi)
                .ok();
        }
        let Some(surface) = target.as_mut() else {
            self.fallback(left);
            return None;
        };
        let width = dip(w as i32, dpi);
        let height = dip(h as i32, dpi);
        let mut region: PAINTSTRUCT = unsafe { std::mem::zeroed() };
        unsafe {
            BeginPaint(child, &mut region);
        }
        surface.begin(paint::BG);
        let mut needed = Vec::new();
        {
            let cache = self.cache.borrow();
            let selection = self.selection.borrow();
            let pixels = self.pixels.borrow();
            if left {
                if let Some(snapshot) = cache
                    .current()
                    .filter(|snapshot| !snapshot.matches.is_empty())
                {
                    let top = unsafe { SendMessageW(child, LB_GETTOPINDEX, 0, 0) }.max(0) as usize;
                    let active = selection.current.as_ref().map(|ticket| ticket.key.id);
                    let focus = if self.list.focus() {
                        self.list.selection()
                    } else {
                        None
                    };
                    for (index, game) in snapshot
                        .matches
                        .iter()
                        .enumerate()
                        .skip(top)
                        .take((height / ui::ROW_HEIGHT).ceil() as usize + 1)
                    {
                        let Some(bounds) = list_rect(child, index) else {
                            break;
                        };
                        ui::paint_match(
                            surface,
                            renderer,
                            width,
                            ui::ListRow {
                                game,
                                y: dip(bounds.top, dpi),
                                active: active == Some(game.id),
                                hover: self.hover.get() == Some(index),
                                focused: focus == Some(index),
                            },
                            &pixels,
                            &mut needed,
                        );
                    }
                } else {
                    surface.text(
                        renderer,
                        "暂无战绩",
                        paint::rect(16.0, 10.0, width - 32.0, 36.0),
                        paint::Font::Base,
                        paint::MUTED,
                    );
                }
            } else {
                let (game, body) = Self::selected(&cache, &selection);
                let legacy = matches!(body, Body::Legacy(_));
                ui::paint_detail(
                    surface,
                    renderer,
                    (width, height),
                    ui::DetailView {
                        game,
                        body,
                        layout: &self.detail_layout.borrow(),
                        state: &selection.detail,
                        notice: selection.error.as_deref().filter(|_| legacy),
                    },
                    &pixels,
                    &mut needed,
                );
            }
        }
        let result = surface.finish();
        unsafe {
            EndPaint(child, &region);
        }
        if result.is_err() {
            *target = None;
            if retried.replace(true) {
                self.fallback(left);
            }
            invalidate(child);
        } else {
            retried.set(false);
            let mut requested = self.image_requested.borrow_mut();
            for key in needed {
                if key != ImageKey::Champion(0) && requested.insert(key) {
                    let _ = self.image_jobs.send(key);
                }
            }
        }
        Some(0)
    }

    fn fallback(&self, left: bool) {
        if left {
            self.list_fallback.set(true);
            let list = hwnd(self.list.handle);
            let (_, _, dpi) = client(list);
            unsafe {
                SendMessageW(list, LB_SETITEMHEIGHT, 0, physical(25, dpi) as isize);
            }
        } else {
            self.detail_fallback.set(true);
            self.detail_frame.set_visible(false);
            self.detail_text.set_visible(true);
            self.refresh_detail();
        }
    }

    fn scroll_detail(&self, delta: f32) {
        let mut selection = self.selection.borrow_mut();
        let height = pane_size(self.detail_frame.handle).1;
        let state = &mut selection.detail;
        state.scroll = (state.scroll + delta).clamp(0.0, self.detail_layout.borrow().limit(height));
        state.hovered = None;
        state.item = None;
        invalidate(hwnd(self.detail_frame.handle));
    }

    fn focus_item(&self, direction: i32) {
        let layout = self.detail_layout.borrow();
        if layout.items.is_empty() {
            return;
        }
        let mut selection = self.selection.borrow_mut();
        let state = &mut selection.detail;
        let next = state.item.map_or(
            if direction < 0 {
                layout.items.len() - 1
            } else {
                0
            },
            |index| (index as i32 + direction).rem_euclid(layout.items.len() as i32) as usize,
        );
        state.item = Some(next);
        state.hovered = None;
        let item = &layout.items[next];
        let height = pane_size(self.detail_frame.handle).1;
        if item.rect.top < state.scroll {
            state.scroll = (item.rect.top - 12.0).max(0.0);
        }
        if item.rect.bottom > state.scroll + height {
            state.scroll = (item.rect.bottom + 12.0 - height).clamp(0.0, layout.limit(height));
        }
        invalidate(hwnd(self.detail_frame.handle));
    }

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
                self.update_busy.set(false);
                self.sync.set_enabled(true);
                self.update_button.set_enabled(true);
                self.status.set_text(error);
            }
            Err(_) => (),
        }
    }

    fn update(&self) {
        if self.sync_busy.get() || self.selection.borrow().working.is_some() {
            self.status.set_text("请等待当前战绩查询完成。");
            return;
        }
        let Some(release) = self.release.borrow().clone() else {
            return;
        };
        self.update_busy.set(true);
        self.update_button.set_enabled(false);
        self.sync.set_enabled(false);
        self.status.set_text("正在下载并验证更新…");
        let (tx, notify) = (self.update_tx.clone(), self.update_notice.sender());
        std::thread::spawn(move || {
            let _ = tx.send(UpdateEvent::Finished(update::stage(&release)));
            notify.notice();
        });
    }
}

fn list_rect(handle: HWND, index: usize) -> Option<RECT> {
    let mut bounds: RECT = unsafe { std::mem::zeroed() };
    let found = unsafe {
        SendMessageW(
            handle,
            LB_GETITEMRECT,
            index,
            (&mut bounds as *mut RECT) as isize,
        )
    };
    (found != -1).then_some(bounds)
}

fn list_hit(handle: HWND, point: isize) -> Option<usize> {
    let result = unsafe { SendMessageW(handle, LB_ITEMFROMPOINT, 0, point) } as u32;
    if result >> 16 != 0 {
        return None;
    }
    let index = (result & 65535) as usize;
    let bounds = list_rect(handle, index)?;
    let (_, y) = mouse(point);
    (y >= bounds.top && y < bounds.bottom).then_some(index)
}

fn bind_panes(app: &Rc<App>) -> Result<Vec<nwg::RawEventHandler>, nwg::NwgError> {
    let weak = Rc::downgrade(app);
    let header = nwg::bind_raw_event_handler(
        &app.window.handle,
        0x10001,
        move |window, message, key, param| {
            let app = weak.upgrade()?;
            let color = |rgb: u32| (rgb & 0xff) << 16 | rgb & 0x00ff00 | rgb >> 16;
            match message {
                WM_ERASEBKGND if !app.header_brush.is_null() => {
                    let mut rect: RECT = unsafe { std::mem::zeroed() };
                    unsafe {
                        GetClientRect(window.cast(), &mut rect);
                        FillRect(key as HWND, &rect, app.header_brush);
                    }
                    Some(1)
                }
                WM_CTLCOLORSTATIC
                    if !app.header_brush.is_null()
                        && param as HWND != hwnd(app.detail_text.handle) =>
                {
                    let foreground = if param as HWND == hwnd(app.account.handle) {
                        paint::TEXT
                    } else {
                        paint::MUTED
                    };
                    unsafe {
                        SetTextColor(key as HWND, color(foreground));
                        SetBkColor(key as HWND, color(paint::BG));
                    }
                    Some(app.header_brush as isize)
                }
                WM_DPICHANGED if param != 0 => {
                    let rect = unsafe { &*(param as *const RECT) };
                    unsafe {
                        SetWindowPos(
                            window.cast(),
                            std::ptr::null_mut(),
                            rect.left,
                            rect.top,
                            rect.right - rect.left,
                            rect.bottom - rect.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                    app.layout();
                    Some(0)
                }
                _ => None,
            }
        },
    )?;
    let mut handlers = vec![header];
    for (left, control, id) in [
        (true, app.list.handle, 0x10002),
        (false, app.detail_frame.handle, 0x10003),
    ] {
        let weak = Rc::downgrade(app);
        handlers.push(nwg::bind_raw_event_handler(
            &control,
            id,
            move |child, message, key, param| {
                let app = weak.upgrade()?;
                pane_message(&app, child.cast(), left, message, key, param)
            },
        )?);
    }
    Ok(handlers)
}

const WM_MOUSELEAVE: u32 = 0x02A3;

fn pane_message(
    app: &App,
    pane: HWND,
    left: bool,
    message: u32,
    key: usize,
    param: isize,
) -> Option<isize> {
    match message {
        WM_PAINT => return app.paint(pane, left),
        WM_ERASEBKGND
            if !(if left {
                app.list_fallback.get()
            } else {
                app.detail_fallback.get()
            }) =>
        {
            return Some(1);
        }
        WM_GETDLGCODE => {
            return Some(
                DLGC_WANTARROWS as isize
                    | if left && key == 13 {
                        DLGC_WANTMESSAGE as isize
                    } else {
                        0
                    },
            );
        }
        _ => (),
    }
    if left {
        match message {
            WM_MOUSEMOVE => {
                leave_tracking(pane);
                let hit = list_hit(pane, param);
                if app.hover.replace(hit) != hit {
                    invalidate(pane);
                }
            }
            WM_LBUTTONDOWN => {
                if let Some(index) = list_hit(pane, param) {
                    unsafe {
                        SetFocus(pane);
                    }
                    app.list.set_selection(Some(index));
                    app.open_detail();
                    invalidate(pane);
                }
                return Some(0);
            }
            WM_MOUSELEAVE => {
                app.hover.set(None);
                invalidate(pane);
            }
            WM_VSCROLL | WM_MOUSEWHEEL | WM_SETFOCUS | WM_KILLFOCUS => invalidate(pane),
            WM_KEYDOWN if key == 13 => {
                app.open_detail();
                return Some(0);
            }
            WM_KEYDOWN if matches!(key, 38 | 40 | 33 | 34 | 35 | 36) => {
                let len = app.list.len();
                if len > 0 {
                    let current = app.list.selection().unwrap_or(0);
                    let index = match key {
                        38 => current.saturating_sub(1),
                        40 => current + 1,
                        33 => current.saturating_sub(6),
                        34 => current + 6,
                        35 => len - 1,
                        _ => 0,
                    };
                    app.list.set_selection(Some(index.min(len - 1)));
                    invalidate(pane);
                }
                return Some(0);
            }
            _ => (),
        }
    } else {
        match message {
            WM_MOUSEMOVE => {
                leave_tracking(pane);
                let (_, _, dpi) = client(pane);
                let (x, y) = mouse(param);
                let state = &mut app.selection.borrow_mut().detail;
                let hovered = app
                    .detail_layout
                    .borrow()
                    .hit_item(dip(x, dpi), dip(y, dpi) + state.scroll);
                if state.hovered != hovered {
                    state.hovered = hovered;
                    invalidate(pane);
                }
            }
            WM_MOUSELEAVE | WM_KILLFOCUS => {
                let mut selection = app.selection.borrow_mut();
                selection.detail.hovered = None;
                if message == WM_KILLFOCUS {
                    selection.detail.item = None;
                }
                invalidate(pane);
            }
            WM_LBUTTONDOWN => {
                unsafe {
                    SetFocus(pane);
                }
                let (_, _, dpi) = client(pane);
                let (x, y) = mouse(param);
                let scroll = app.selection.borrow().detail.scroll;
                app.profile_at(dip(x, dpi), dip(y, dpi) + scroll);
                return Some(0);
            }
            WM_MOUSEWHEEL => {
                app.scroll_detail(-((key >> 16) as i16 as f32) * 0.75);
                return Some(0);
            }
            WM_KEYDOWN if matches!(key, 37 | 39) => {
                app.focus_item(if key == 37 { -1 } else { 1 });
                return Some(0);
            }
            WM_KEYDOWN if matches!(key, 38 | 40 | 33 | 34) => {
                app.scroll_detail(match key {
                    38 => -58.0,
                    40 => 58.0,
                    33 => -300.0,
                    _ => 300.0,
                });
                return Some(0);
            }
            _ => (),
        }
    }
    None
}

fn query_worker(
    jobs: mpsc::Receiver<(Request, Option<Ticket>)>,
    tx: mpsc::Sender<(Option<Ticket>, Reply)>,
    notice: nwg::NoticeSender,
) {
    let mut connection: Option<elevation::Connection> = None;
    for (request, ticket) in jobs {
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
                Reply::AccessDenied => Reply::Error("请点击「连接并同步」建立连接。".into()),
                reply => reply,
            }
        };
        if tx.send((ticket, reply)).is_err() {
            break;
        }
        notice.notice();
    }
}

struct ComApartment(bool);
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                windows::Win32::System::Com::CoUninitialize();
            }
        }
    }
}

fn gui(update_ready: bool) -> Result<(), nwg::NwgError> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let _com = ComApartment(
        unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            )
        }
        .is_ok(),
    );
    nwg::init()?;
    nwg::Font::set_global_family("Microsoft YaHei UI")?;
    let (tx, rx) = mpsc::channel();
    let (requests, jobs) = mpsc::channel();
    let (image_jobs, image_work) = mpsc::channel();
    let (image_tx, image_results) = mpsc::channel();
    let (cache_progress_tx, cache_progress_rx) = mpsc::channel();
    let (update_tx, update_rx) = mpsc::channel();
    let renderer = Renderer::new().ok();
    let fallback = renderer.is_none();
    let mut app = App {
        window: Default::default(),
        sync: Default::default(),
        update_button: Default::default(),
        status: Default::default(),
        account: Default::default(),
        ranks: Default::default(),
        matches_heading: Default::default(),
        detail_heading: Default::default(),
        account_font: Default::default(),
        section_font: Default::default(),
        list: Default::default(),
        rows: Default::default(),
        dpi: Default::default(),
        body_font: Default::default(),
        detail_frame: Default::default(),
        detail_text: Default::default(),
        cache: RefCell::new(Cache::load()),
        selection: Default::default(),
        detail_layout: RefCell::new(DetailLayout::new(720.0, &Body::Empty(""))),
        renderer,
        list_surface: Default::default(),
        detail_surface: Default::default(),
        list_fallback: Cell::new(fallback),
        detail_fallback: Cell::new(fallback),
        hover: Default::default(),
        detail_repainted: Default::default(),
        list_repainted: Default::default(),
        sync_busy: Default::default(),
        update_busy: Default::default(),
        header_brush: unsafe { CreateSolidBrush(0x241611) },
        pixels: Default::default(),
        image_requested: Default::default(),
        image_notice: Default::default(),
        cache_progress: Default::default(),
        cache_progress_rx,
        image_jobs,
        image_results,
        update_notice: Default::default(),
        update_tx,
        update_rx,
        release: Default::default(),
        notice: Default::default(),
        requests,
        rx,
    };
    let dpi = unsafe { GetDpiForSystem() }.max(96);
    nwg::Window::builder()
        .size((physical(1120, dpi), physical(700, dpi)))
        .center(true)
        .title("国服战绩查询")
        .flags(nwg::WindowFlags::MAIN_WINDOW)
        .build(&mut app.window)?;
    // Shared handles are owned by Windows and remain valid for the module's lifetime.
    unsafe {
        let module = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
        for (kind, size) in [(ICON_SMALL, 16), (ICON_BIG, 32)] {
            let icon = LoadImageW(
                module,
                std::ptr::with_exposed_provenance::<u16>(1),
                IMAGE_ICON,
                physical(size, dpi),
                physical(size, dpi),
                LR_SHARED,
            );
            if !icon.is_null() {
                SendMessageW(
                    hwnd(app.window.handle),
                    WM_SETICON,
                    kind as usize,
                    icon as isize,
                );
            }
        }
    }
    nwg::Button::builder()
        .text("连接并同步")
        .size((134, 36))
        .parent(&app.window)
        .build(&mut app.sync)?;
    nwg::Button::builder()
        .text("检查更新")
        .size((122, 36))
        .parent(&app.window)
        .build(&mut app.update_button)?;
    app.update_button.set_visible(false);
    nwg::Label::builder()
        .position((24, 12))
        .parent(&app.window)
        .build(&mut app.account)?;
    nwg::Label::builder()
        .position((25, 50))
        .parent(&app.window)
        .build(&mut app.ranks)?;
    nwg::Label::builder()
        .position((25, 74))
        .parent(&app.window)
        .build(&mut app.status)?;
    nwg::Label::builder()
        .text("近期战绩")
        .position((24, 98))
        .size((160, 22))
        .parent(&app.window)
        .build(&mut app.matches_heading)?;
    nwg::Label::builder()
        .text("对局详情")
        .size((160, 22))
        .parent(&app.window)
        .build(&mut app.detail_heading)?;
    nwg::ListBox::builder()
        .position((24, 120))
        .size((320, 556))
        .parent(&app.window)
        .build(&mut app.list)?;
    nwg::Frame::builder()
        .parent(&app.window)
        .build(&mut app.detail_frame)?;
    let panel = hwnd(app.detail_frame.handle);
    unsafe {
        let style = GetWindowLongPtrW(panel, GWL_STYLE);
        SetWindowLongPtrW(panel, GWL_STYLE, style | WS_TABSTOP as isize);
    }
    nwg::TextBox::builder()
        .readonly(true)
        .flags(
            nwg::TextBoxFlags::VISIBLE
                | nwg::TextBoxFlags::VSCROLL
                | nwg::TextBoxFlags::AUTOVSCROLL,
        )
        .parent(&app.window)
        .build(&mut app.detail_text)?;
    app.detail_text.set_visible(fallback);
    app.detail_frame.set_visible(!fallback);
    for notice in [
        &mut app.notice,
        &mut app.image_notice,
        &mut app.update_notice,
    ] {
        nwg::Notice::builder().parent(&app.window).build(notice)?;
    }
    nwg::Label::builder()
        .text("检查图片缓存…")
        .size((306, 22))
        .parent(&app.window)
        .build(&mut app.cache_progress)?;
    let app = Rc::new(app);
    let weak = Rc::downgrade(&app);
    let handler = nwg::full_bind_event_handler(&app.window.handle, move |event, data, handle| {
        use nwg::Event::*;
        let Some(app) = weak.upgrade() else { return };
        match event {
            OnWindowClose if handle == app.window => nwg::stop_thread_dispatch(),
            OnMinMaxInfo if handle == app.window => {
                let (_, _, dpi) = client(hwnd(app.window.handle));
                data.on_min_max()
                    .set_min_size(physical(780, dpi), physical(520, dpi));
            }
            OnResize | OnWindowMaximize if handle == app.window => app.layout(),
            OnButtonClick if handle == app.sync => app.sync(),
            OnListBoxSelect | OnListBoxDoubleClick if handle == app.list => app.open_detail(),
            OnNotice if handle == app.notice => app.finish(),
            OnNotice if handle == app.image_notice => app.image_arrived(),
            OnNotice if handle == app.update_notice => app.update_event(),
            OnButtonClick if handle == app.update_button => app.update(),
            _ => (),
        }
    });
    let raw = bind_panes(&app)?;
    app.layout();
    app.load_snapshot(false);
    let (network_tx, network_rx) = mpsc::channel();
    let network_rx = std::sync::Arc::new(std::sync::Mutex::new(network_rx));
    for _ in 0..2 {
        let jobs = network_rx.clone();
        let results = image_tx.clone();
        let notice = app.image_notice.sender();
        std::thread::spawn(move || {
            let agent = assets::portrait_client();
            loop {
                let job = jobs.lock().ok().and_then(|receiver| receiver.recv().ok());
                let Some(key) = job else { break };
                if results.send((key, assets::image(&agent, key))).is_err() {
                    break;
                }
                notice.notice();
            }
        });
    }
    let disk_notice = app.image_notice.sender();
    std::thread::spawn(move || {
        for key in image_work {
            if let Some(bytes) = assets::cached_image(key) {
                if image_tx.send((key, Some(bytes))).is_err() {
                    break;
                }
                disk_notice.notice();
            } else if network_tx.send(key).is_err() {
                break;
            }
        }
    });
    let image_notice = app.image_notice.sender();
    std::thread::spawn(move || {
        let agent = assets::portrait_client();
        let all = assets::initialization_keys();
        let total = all.len();
        let mut pending = std::collections::VecDeque::new();
        let mut cached = 0;
        for key in all {
            if assets::is_cached(key) {
                cached += 1;
            } else {
                pending.push_back(key);
            }
        }
        let mut failures = 0;
        let mut consecutive_failures = 0;
        let publish = |message: String| {
            let _ = cache_progress_tx.send(message);
            image_notice.notice();
        };
        publish(if pending.is_empty() {
            format!("图片缓存已就绪 · {cached}")
        } else {
            format!("初始化图片缓存 · {cached}/{total}")
        });
        while let Some(key) = pending.pop_front() {
            // Cache initialization never shares a queue with visible image reads.

            if assets::is_cached(key)
                || (assets::image(&agent, key).is_some() && assets::is_cached(key))
            {
                cached += 1;
                consecutive_failures = 0;
            } else {
                failures += 1;
                consecutive_failures += 1;
            }
            if consecutive_failures >= 4 {
                pending.clear();
                publish(format!("图片缓存 {cached}/{total} · 下次启动继续"));
            } else if pending.is_empty() {
                publish(if failures == 0 {
                    format!("图片缓存已就绪 · {cached}")
                } else {
                    format!("图片缓存 {cached}/{total} · 下次补齐")
                });
            } else if (cached + failures) % 10 == 0 {
                publish(format!("初始化图片缓存 · {cached}/{total}"));
            }
        }
    });
    let notice = app.notice.sender();
    std::thread::spawn(move || query_worker(jobs, tx, notice));
    app.window.set_visible(true);
    unsafe {
        windows_sys::Win32::Graphics::Gdi::UpdateWindow(hwnd(app.list.handle));
        windows_sys::Win32::Graphics::Gdi::UpdateWindow(hwnd(app.detail_frame.handle));
    }
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
    app.sync();
    nwg::dispatch_thread_events();
    for handle in raw {
        let _ = nwg::unbind_raw_event_handler(&handle);
    }
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

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn reads_legacy_details_and_preserves_them_on_save() {
        let old = r#"{"latest":null,"accounts":{},"details":{"a:1":"旧详情"}}"#;
        let cache: Cache = serde_json::from_str(old).unwrap();
        assert!(cache.structured_details.is_empty());
        assert_eq!(cache.details["a:1"], "旧详情");
        let reread: Cache = serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        assert_eq!(reread.details["a:1"], "旧详情");
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::*;

    fn key(id: u64) -> MatchKey {
        MatchKey {
            account: "HN1:fixture".into(),
            id,
        }
    }

    #[test]
    fn pending_selection_replaces_intermediate_click_and_late_reply_stays_stale() {
        let mut selection = Selection::default();
        selection.select(Some(key(1)), true);
        let first = selection.next().unwrap();
        selection.select(Some(key(1)), true);
        assert!(!selection.want_query);
        selection.select(Some(key(2)), true);
        selection.select(Some(key(3)), true);
        assert!(selection.next().is_none());
        assert!(!selection.complete(&first));
        let latest = selection.next().unwrap();
        assert_eq!(latest.key.id, 3);
        assert!(latest.generation > first.generation);
        assert!(selection.complete(&latest));
    }

    #[test]
    fn hit_coordinates_remain_aligned_at_standard_dpi_scales() {
        for dpi in [96, 120, 144] {
            let y = 64;
            assert_eq!(dip(physical(y, dpi), dpi), y as f32);
        }
    }

    #[test]
    fn narrow_layout_keeps_trinket_hit_rectangle_distinct() {
        let detail = Detail {
            game_id: 1,
            duration_seconds: None,
            teams: vec![300],
            self_id: Some(1),
            platform: None,
            account_puuid: None,
            participants: vec![lcu::Participant {
                summoner_name: None,
                tag_line: None,
                puuid: None,
                participant_id: Some(1),
                team_id: Some(300),
                champion_id: Some(22),
                win: None,
                kills: Some(0),
                deaths: None,
                assists: None,
                cs: None,
                items: [Some(1001), None, None, None, None, None, Some(3340)],
            }],
        };
        for width in [380.0, 700.0] {
            let layout = DetailLayout::new(width, &Body::Structured(&detail));
            let [boots, trinket] = layout.items.as_slice() else {
                panic!("two populated slots")
            };
            assert!(boots.rect.right < trinket.rect.left);
            assert!(trinket.rect.right < width);
            assert_eq!(
                layout.hit_item(trinket.rect.left + 2.0, trinket.rect.top + 2.0),
                Some(1)
            );
            assert!(trinket.trinket);
        }
    }
}
