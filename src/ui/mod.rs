pub mod paint;

use crate::{
    assets::{self, ImageKey},
    lcu::{Detail, Match},
    time,
};
use paint::{
    ACCENT, BG, BORDER, CARD, Font, LOSS, MUTED, PANEL, Pixels, Renderer, Surface, TEXT, WIN, rect,
};
use std::collections::HashMap;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

pub const ROW_HEIGHT: f32 = 64.0;
const DETAIL_HEADER: f32 = 118.0;

#[derive(Default)]
pub struct DetailState {
    pub scroll: f32,
    pub item: Option<usize>,
    pub hovered: Option<usize>,
    pub player_hover: Option<usize>,
}

impl DetailState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

pub enum Body<'a> {
    Empty(&'a str),
    Legacy(&'a str),
    Structured(&'a Detail),
}

pub struct DetailLayout {
    rows: Vec<Row>,
    pub height: f32,
    pub items: Vec<Slot>,
    pub players: Vec<(D2D_RECT_F, usize)>,
}

struct Row {
    y: f32,
    h: f32,
    kind: RowKind,
}
enum RowKind {
    Team(Option<u64>),
    Player(usize),
    Line(String),
}

pub struct Slot {
    pub rect: D2D_RECT_F,
    pub key: ImageKey,
    pub label: String,
    pub trinket: bool,
    pub slot: usize,
}

impl DetailLayout {
    pub fn new(width: f32, body: &Body<'_>) -> Self {
        let mut layout = Self {
            rows: Vec::new(),
            height: DETAIL_HEADER,
            items: Vec::new(),
            players: Vec::new(),
        };
        match body {
            Body::Empty(_) => layout.height += 66.0,
            Body::Legacy(text) => {
                let span = ((width - 42.0) / 14.0).max(12.0) as usize;
                for line in text.lines() {
                    let chars: Vec<_> = line.chars().collect();
                    if chars.is_empty() {
                        layout.height += 10.0;
                    }
                    for chunk in chars.chunks(span) {
                        layout.push(24.0, RowKind::Line(chunk.iter().collect()));
                    }
                }
            }
            Body::Structured(detail) => {
                for team in detail.teams.iter().copied().map(Some).chain([None]) {
                    let players: Vec<_> = detail
                        .participants
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| p.team_id == team)
                        .collect();
                    if players.is_empty() {
                        continue;
                    }
                    layout.push(34.0, RowKind::Team(team));
                    for (index, player) in players {
                        let row_top = layout.height;
                        let numbers = width - 166.0;
                        layout
                            .players
                            .push((rect(70.0, row_top + 4.0, numbers - 76.0, 37.0), index));
                        layout.push(114.0, RowKind::Player(index));
                        for (slot, id) in player.items.iter().enumerate() {
                            if let Some(id) = id {
                                layout.items.push(Slot {
                                    rect: item_rect(width, row_top, slot),
                                    key: ImageKey::Item(*id),
                                    label: assets::item_name(*id),
                                    trinket: slot == 6,
                                    slot,
                                });
                            }
                        }
                        for (key, bounds, label) in combat_slots(width, row_top, player) {
                            layout.items.push(Slot {
                                rect: bounds,
                                key,
                                label,
                                trinket: false,
                                slot: 0,
                            });
                        }
                    }
                    layout.height += 10.0;
                }
            }
        }
        layout.height += 18.0;
        layout
    }

    fn push(&mut self, height: f32, kind: RowKind) {
        self.rows.push(Row {
            y: self.height,
            h: height,
            kind,
        });
        self.height += height;
    }

    pub fn limit(&self, viewport: f32) -> f32 {
        (self.height - viewport).max(0.0)
    }

    pub fn hit_item(&self, x: f32, y: f32) -> Option<usize> {
        self.items.iter().position(|slot| {
            x >= slot.rect.left && x < slot.rect.right && y >= slot.rect.top && y < slot.rect.bottom
        })
    }
}

fn item_rect(width: f32, y: f32, index: usize) -> D2D_RECT_F {
    let _ = width;
    let (left, top) = (70.0, y + 80.0);
    rect(
        left + index as f32 * 31.0 + if index == 6 { 7.0 } else { 0.0 },
        top,
        27.0,
        27.0,
    )
}

fn compact(value: Option<u64>) -> String {
    value
        .map(|v| {
            if v >= 1000 {
                format!("{:.1}k", v as f32 / 1000.0)
            } else {
                v.to_string()
            }
        })
        .unwrap_or_else(|| "—".into())
}

fn team_summary(detail: &Detail, team: Option<u64>) -> String {
    let players: Vec<_> = detail
        .participants
        .iter()
        .filter(|p| p.team_id == team)
        .collect();
    let outcome = players
        .iter()
        .find_map(|p| p.win)
        .map(|win| if win { "胜方" } else { "败方" })
        .unwrap_or("阵容");
    let sum = |read: fn(&crate::lcu::Participant) -> Option<u64>| {
        players
            .iter()
            .try_fold(0u64, |n, p| n.checked_add(read(p)?))
    };
    format!(
        "{outcome} · {}    {} / {} / {}    经济 {}",
        if detail.context.is_some() && detail.teams == [300] {
            "我方队友".into()
        } else {
            assets::team_name(team, detail.teams == [100, 200])
        },
        assets::stat(sum(|p| p.kills)),
        assets::stat(sum(|p| p.deaths)),
        assets::stat(sum(|p| p.assists)),
        compact(sum(|p| p.gold))
    )
}
fn combat_slots(
    width: f32,
    y: f32,
    p: &crate::lcu::Participant,
) -> Vec<(ImageKey, D2D_RECT_F, String)> {
    let mut slots = Vec::new();
    if let Some(rank) = &p.rank
        && rank.tier > 0
    {
        slots.push((
            ImageKey::Rank(rank.tier),
            rect(23.0, y + 79.0, 28.0, 28.0),
            format!("{} · 查询 {}", rank.label, time(rank.checked_at)),
        ));
    }
    for (index, id) in p.spells.iter().enumerate() {
        if let Some(id) = id {
            let key = ImageKey::Spell(*id);
            slots.push((
                key,
                rect(width - 136.0 + index as f32 * 27.0, y + 46.0, 23.0, 23.0),
                assets::image_name(key),
            ));
        }
    }
    for (index, id) in p.runes.iter().enumerate() {
        if let Some(id) = id {
            let key = ImageKey::Rune(*id);
            slots.push((
                key,
                rect(width - 78.0 + index as f32 * 27.0, y + 46.0, 23.0, 23.0),
                assets::image_name(key),
            ));
        }
    }
    slots
}

pub fn result(win: Option<bool>) -> (&'static str, u32) {
    match win {
        Some(true) => ("胜利", WIN),
        Some(false) => ("失败", LOSS),
        None => ("—", MUTED),
    }
}

pub fn list_label(game: &Match) -> String {
    let (outcome, _) = result(game.stats.as_ref().map(|stats| stats.win));
    let kda = game
        .stats
        .as_ref()
        .map(|s| format!("{}/{}/{}", s.kills, s.deaths, s.assists))
        .unwrap_or_else(|| "—".into());
    format!(
        "{}  {}  {}  {}  {}",
        assets::champion_name(game.champion),
        outcome,
        kda,
        assets::mode_name(&game.mode),
        time(game.timestamp)
    )
}

pub struct ListRow<'a> {
    pub game: &'a Match,
    pub y: f32,
    pub active: bool,
    pub hover: bool,
    pub focused: bool,
}

pub fn paint_match(
    surface: &mut Surface,
    renderer: &Renderer,
    width: f32,
    row: ListRow<'_>,
    images: &HashMap<ImageKey, Pixels>,
    needed: &mut Vec<ImageKey>,
) {
    let ListRow {
        game,
        y,
        active,
        hover,
        focused,
    } = row;
    let (outcome, tint) = result(game.stats.as_ref().map(|s| s.win));
    let bounds = rect(5.0, y + 3.0, width - 10.0, 57.0);
    surface.fill(
        bounds,
        if active {
            0x25314c
        } else if hover {
            CARD
        } else {
            PANEL
        },
        8.0,
    );
    if active || focused {
        surface.border(bounds, ACCENT, 8.0);
    }
    surface.fill(rect(7.0, y + 15.0, 3.0, 33.0), tint, 1.5);
    let key = ImageKey::Champion(game.champion);
    draw_image(
        surface,
        key,
        rect(20.0, y + 12.0, 40.0, 40.0),
        images,
        needed,
    );
    surface.text(
        renderer,
        &assets::champion_name(game.champion),
        rect(70.0, y + 9.0, width - 162.0, 23.0),
        Font::Strong,
        TEXT,
    );
    surface.text(
        renderer,
        &format!(
            "{} · {}",
            assets::mode_name(&game.mode),
            time(game.timestamp)
        ),
        rect(70.0, y + 33.0, width - 159.0, 20.0),
        Font::Small,
        MUTED,
    );
    surface.text(
        renderer,
        outcome,
        rect(width - 80.0, y + 9.0, 71.0, 23.0),
        Font::Strong,
        tint,
    );
    let kda = game
        .stats
        .as_ref()
        .map(|s| format!("{}/{}/{}", s.kills, s.deaths, s.assists))
        .unwrap_or_else(|| "—".into());
    surface.text(
        renderer,
        &kda,
        rect(width - 80.0, y + 33.0, 71.0, 20.0),
        Font::Small,
        TEXT,
    );
}

fn draw_image(
    surface: &mut Surface,
    key: ImageKey,
    bounds: D2D_RECT_F,
    images: &HashMap<ImageKey, Pixels>,
    needed: &mut Vec<ImageKey>,
) {
    let pixels = images.get(&key);
    surface.image(key, pixels, bounds);
    if pixels.is_none() {
        needed.push(key);
    }
}

pub struct DetailView<'a> {
    pub game: Option<&'a Match>,
    pub body: Body<'a>,
    pub layout: &'a DetailLayout,
    pub state: &'a DetailState,
    pub notice: Option<&'a str>,
}

pub fn paint_detail(
    surface: &mut Surface,
    renderer: &Renderer,
    viewport: (f32, f32),
    view: DetailView<'_>,
    images: &HashMap<ImageKey, Pixels>,
    needed: &mut Vec<ImageKey>,
) {
    let (width, height) = viewport;
    let DetailView {
        game,
        body,
        layout,
        state,
        notice,
    } = view;
    let offset = state.scroll.min(layout.limit(height));
    let title_width = width - 36.0;
    if let Some(game) = game {
        let wins = game.stats.as_ref().map(|s| s.win).or_else(|| {
            let Body::Structured(detail) = &body else {
                return None;
            };
            let id = detail.self_id?;
            detail
                .participants
                .iter()
                .find(|p| p.participant_id == Some(id))?
                .win
        });
        let (outcome, tint) = result(wins);
        if offset < DETAIL_HEADER {
            surface.fill(rect(12.0, 10.0 - offset, width - 24.0, 95.0), PANEL, 10.0);
            surface.text(
                renderer,
                outcome,
                rect(25.0, 18.0 - offset, 92.0, 32.0),
                Font::Title,
                tint,
            );
            let summary = format!(
                "{} · {}",
                assets::mode_name(&game.mode),
                time(game.timestamp)
            );
            surface.text(
                renderer,
                &summary,
                rect(26.0, 49.0 - offset, title_width - 12.0, 21.0),
                Font::Base,
                TEXT,
            );
            let stats = if let Some(notice) = notice {
                notice.to_owned()
            } else if let Body::Structured(detail) = &body {
                let duration = detail
                    .duration_seconds
                    .map(|s| format!("{}:{:02}", s / 60, s % 60))
                    .unwrap_or_else(|| "—".into());
                match detail.score() {
                    Some((blue, red)) => {
                        format!("时长 {duration}    •    击杀  蓝方 {blue} : {red} 红方")
                    }
                    None => format!("时长 {duration}"),
                }
            } else {
                "时长 —".into()
            };
            surface.text(
                renderer,
                &stats,
                rect(26.0, 73.0 - offset, title_width - 12.0, 22.0),
                Font::Small,
                MUTED,
            );
        }
    }

    if game.is_none()
        && let Body::Structured(detail) = &body
    {
        surface.fill(rect(12.0, 10.0 - offset, width - 24.0, 95.0), PANEL, 10.0);
        surface.text(
            renderer,
            detail.context.as_deref().unwrap_or("当前对局"),
            rect(25.0, 22.0 - offset, width - 50.0, 28.0),
            Font::Strong,
            TEXT,
        );
        surface.text(
            renderer,
            "点击玩家查战绩 · 段位为当前查询结果 · 未提供的字段显示 —",
            rect(25.0, 58.0 - offset, width - 50.0, 40.0),
            Font::Small,
            MUTED,
        );
    }
    if let Body::Empty(message) = &body {
        let top = if game.is_some() { DETAIL_HEADER } else { 22.0 };
        surface.text(
            renderer,
            message,
            rect(25.0, top - offset, title_width - 12.0, 44.0),
            Font::Base,
            MUTED,
        );
    }

    let first = layout.rows.partition_point(|row| row.y + row.h <= offset);
    for row in layout.rows[first..]
        .iter()
        .take_while(|row| row.y < offset + height)
    {
        let y = row.y - offset;
        match &row.kind {
            RowKind::Line(text) => surface.text(
                renderer,
                text,
                rect(25.0, y, title_width - 12.0, row.h),
                Font::Base,
                TEXT,
            ),
            RowKind::Team(team) => surface.text(
                renderer,
                &match &body {
                    Body::Structured(detail) => team_summary(detail, *team),
                    _ => assets::team_name(*team, false),
                },
                rect(23.0, y, title_width, row.h),
                Font::Strong,
                match team {
                    Some(100) => ACCENT,
                    Some(200) => LOSS,
                    _ => MUTED,
                },
            ),
            RowKind::Player(index) => {
                let Body::Structured(detail) = &body else {
                    continue;
                };
                let player = &detail.participants[*index];
                let me = detail.self_id.is_some() && detail.self_id == player.participant_id;
                let card = rect(12.0, y + 2.0, width - 24.0, row.h - 4.0);
                surface.fill(card, PANEL, 7.0);
                if me {
                    surface.border(card, ACCENT, 7.0);
                }
                let key = ImageKey::Champion(player.champion_id.unwrap_or_default());
                draw_image(
                    surface,
                    key,
                    rect(23.0, y + 8.0, 35.0, 35.0),
                    images,
                    needed,
                );
                if state.player_hover == Some(*index) {
                    surface.fill(rect(65.0, y + 2.0, width - 242.0, 41.0), CARD, 5.0);
                }
                let name = format!(
                    "{}{}",
                    player
                        .summoner_name
                        .as_deref()
                        .unwrap_or("召唤师信息待刷新"),
                    if me { "  我" } else { "" }
                );
                let numbers = width - 166.0;
                surface.text(
                    renderer,
                    &name,
                    rect(70.0, y + 2.0, numbers - 76.0, 23.0),
                    Font::Base,
                    if player.puuid.as_deref().is_some_and(crate::lcu::valid_puuid) {
                        ACCENT
                    } else {
                        TEXT
                    },
                );
                let identity = player
                    .tag_line
                    .as_ref()
                    .map(|tag| format!("#{tag}"))
                    .or_else(|| player.puuid.clone())
                    .unwrap_or_else(|| "标识待刷新".into());
                surface.text(
                    renderer,
                    &identity,
                    rect(70.0, y + 24.0, numbers - 76.0, 17.0),
                    Font::Small,
                    MUTED,
                );
                let kda = format!(
                    "{} / {} / {}",
                    assets::stat(player.kills),
                    assets::stat(player.deaths),
                    assets::stat(player.assists)
                );
                surface.text(
                    renderer,
                    &kda,
                    rect(numbers, y + 5.0, 86.0, 34.0),
                    Font::Strong,
                    TEXT,
                );
                surface.text(
                    renderer,
                    &format!("补刀 {}", assets::stat(player.cs)),
                    rect(numbers + 88.0, y + 5.0, 65.0, 34.0),
                    Font::Small,
                    MUTED,
                );
                let economy = format!("金 {}  伤 {}", compact(player.gold), compact(player.damage));
                surface.text(
                    renderer,
                    &economy,
                    rect(numbers, y + 29.0, 153.0, 20.0),
                    Font::Small,
                    MUTED,
                );
                for (key, bounds, _) in combat_slots(width, y, player) {
                    draw_image(surface, key, bounds, images, needed);
                }
                let rank_label = player.rank.as_ref().map(|r| r.label.as_str()).unwrap_or(
                    if player.puuid.as_deref().is_some_and(crate::lcu::valid_puuid) {
                        "当前段位待查询"
                    } else {
                        "当前段位未提供"
                    },
                );
                surface.text(
                    renderer,
                    rank_label,
                    rect(23.0, y + 43.0, (width - 186.0).max(90.0), 17.0),
                    Font::Small,
                    MUTED,
                );
                for (slot, id) in player.items.iter().enumerate() {
                    let bounds = item_rect(width, y, slot);
                    if let Some(id) = id {
                        draw_image(surface, ImageKey::Item(*id), bounds, images, needed);
                    } else {
                        surface.fill(bounds, CARD, 4.0);
                    }
                    if slot == 6 {
                        surface.border(bounds, BORDER, 4.0);
                    }
                }
            }
        }
    }

    if layout.height > height && height > 20.0 {
        let rail = height - 12.0;
        let thumb = (height / layout.height * rail).max(20.0);
        let top = 6.0 + offset / layout.limit(height) * (rail - thumb);
        surface.fill(rect(width - 6.0, top, 3.0, thumb), BORDER, 1.5);
    }
    if let Some(index) = state.hovered.or(state.item)
        && let Some(slot) = layout.items.get(index)
    {
        let mut bounds = slot.rect;
        bounds.top -= offset;
        bounds.bottom -= offset;
        if bounds.bottom > 0.0 && bounds.top < height {
            surface.border(bounds, ACCENT, 4.0);
            let label = if slot.trinket {
                format!("饰品 · {}", slot.label)
            } else if matches!(slot.key, ImageKey::Item(_)) {
                format!("装备槽 {} · {}", slot.slot + 1, slot.label)
            } else {
                slot.label.clone()
            };
            let top = if bounds.top > 52.0 {
                bounds.top - 35.0
            } else {
                bounds.bottom + 5.0
            };
            let left = bounds.left.min((width - 230.0).max(6.0));
            surface.fill(rect(left, top, 222.0, 29.0), BG, 6.0);
            surface.border(rect(left, top, 222.0, 29.0), BORDER, 6.0);
            surface.text(
                renderer,
                &label,
                rect(left + 8.0, top, 208.0, 29.0),
                Font::Small,
                TEXT,
            );
        }
    }
}
