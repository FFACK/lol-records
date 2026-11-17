use serde::Deserialize;
use std::{collections::HashMap, io::Read, path::PathBuf, sync::LazyLock, time::Duration};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

#[derive(Deserialize)]
struct Reference {
    version: String,
    champions: HashMap<u64, (String, String)>,
    items: HashMap<u64, String>,
}

static REFERENCE: LazyLock<Reference> = LazyLock::new(|| {
    serde_json::from_str(include_str!("reference.json").trim_start_matches('\u{feff}'))
        .expect("valid bundled Data Dragon names")
});

pub fn champion_name(id: u64) -> String {
    REFERENCE
        .champions
        .get(&id)
        .map(|hero| hero.0.clone())
        .unwrap_or_else(|| {
            if id == 0 {
                "未知英雄".into()
            } else {
                format!("未知英雄 #{id}")
            }
        })
}

pub fn item_name(id: u64) -> String {
    REFERENCE
        .items
        .get(&id)
        .cloned()
        .unwrap_or_else(|| format!("未知装备 #{id}"))
}

pub fn team_name(id: Option<u64>) -> String {
    match id {
        Some(100) => "蓝方".into(),
        Some(200) => "红方".into(),
        Some(id) => format!("队伍 {id}"),
        None => "队伍未知".into(),
    }
}

pub fn stat(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "—".into())
}

pub fn structured_text(detail: &crate::lcu::Detail) -> String {
    let mut text = detail
        .duration_seconds
        .map(|seconds| format!("对局时长 {}:{:02}", seconds / 60, seconds % 60))
        .unwrap_or_else(|| "对局时长 —".into());
    if let Some((blue, red)) = detail.score() {
        text.push_str(&format!("     蓝方 {blue}  /  红方 {red}"));
    }
    for team in detail.teams.iter().copied().map(Some).chain([None]) {
        let mut players = detail
            .participants
            .iter()
            .filter(|player| player.team_id == team)
            .peekable();
        if players.peek().is_none() {
            continue;
        }
        text.push_str(&format!(
            "\r\n\r\n{} · 英雄 / KDA / 补刀 / 装备",
            team_name(team)
        ));
        for player in players {
            let me = if detail.self_id.is_some() && detail.self_id == player.participant_id {
                "（我）"
            } else {
                ""
            };
            let items = player
                .items
                .iter()
                .flatten()
                .copied()
                .map(item_name)
                .collect::<Vec<_>>()
                .join("、");
            text.push_str(&format!(
                "\r\n{}{me}    {} / {} / {}    补刀 {}    {items}",
                champion_name(player.champion_id.unwrap_or_default()),
                stat(player.kills),
                stat(player.deaths),
                stat(player.assists),
                stat(player.cs)
            ));
        }
    }
    text
}

pub fn mode_name(mode: &str) -> &str {
    match mode {
        "CLASSIC" => "召唤师峡谷",
        "ARAM" => "极地大乱斗",
        "KIWI" => "海克斯大乱斗",
        "CHERRY" => "斗魂竞技场",
        "URF" => "无限火力",
        "SWIFTPLAY" => "快速模式",
        "JADE" => "经典模式",
        "" => "未知模式",
        _ => mode,
    }
}

pub fn detail_text(raw: &str) -> String {
    let mut lines = raw.lines();
    let title = lines.next().unwrap_or_default();
    let mut teams = [Vec::new(), Vec::new()];
    let mut others = Vec::new();
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let columns: Vec<_> = line.splitn(5, "  ").collect();
        let [team, hero, kda, cs, equipment] = columns.as_slice() else {
            others.push(line.to_owned());
            continue;
        };
        let id = hero.strip_prefix("英雄 ").and_then(|id| id.parse().ok());
        let name = id.map(champion_name).unwrap_or_else(|| (*hero).into());
        let items = equipment
            .strip_prefix("装备 ")
            .unwrap_or_default()
            .split_whitespace()
            .map(|id| {
                id.parse::<u64>()
                    .ok()
                    .and_then(|key| REFERENCE.items.get(&key))
                    .cloned()
                    .unwrap_or_else(|| format!("未知装备 #{id}"))
            })
            .collect::<Vec<_>>()
            .join("、");
        let row = format!(
            "{name}    {kda}    {cs}{}",
            if items.is_empty() {
                String::new()
            } else {
                format!("    {items}")
            }
        );
        match *team {
            "蓝方" => teams[0].push(row),
            "红方" => teams[1].push(row),
            _ => others.push(row),
        }
    }
    let mut sections = vec![title.to_owned()];
    for (name, rows) in ["蓝方", "红方"].into_iter().zip(teams) {
        if !rows.is_empty() {
            sections.push(format!(
                "{name} · 英雄 / KDA / 补刀 / 装备\r\n{}",
                rows.join("\r\n")
            ));
        }
    }
    sections.extend(others);
    sections.join("\r\n\r\n")
}

pub fn portrait_client() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(4)))
        .max_redirects(0)
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent()
}

pub fn portrait(agent: &ureq::Agent, id: u64) -> Option<Vec<u8>> {
    const MAX_BYTES: usize = 256 * 1024;
    let (_, filename) = REFERENCE.champions.get(&id)?;
    let path = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|root| {
            root.join("LolRecords/portraits")
                .join(&REFERENCE.version)
                .join(format!("{id}.png"))
        });
    let valid_png = |data: &[u8]| data.len() <= MAX_BYTES && data.starts_with(b"\x89PNG\r\n\x1a\n");
    if let Some(path) = &path
        && let Ok(file) = std::fs::File::open(path)
    {
        let mut data = Vec::new();
        if file
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut data)
            .is_ok()
            && valid_png(&data)
        {
            return Some(data);
        }
    }
    let mut response = agent
        .get(format!(
            "https://ddragon.leagueoflegends.com/cdn/{}/img/champion/{filename}",
            REFERENCE.version
        ))
        .call()
        .ok()?;
    let mut data = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .ok()?;
    if !valid_png(&data) {
        return None;
    }
    if let Some(path) = path
        && let Some(folder) = path.parent()
        && std::fs::create_dir_all(folder).is_ok()
    {
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&tmp, &data)
            .and_then(|_| std::fs::rename(&tmp, &path))
            .is_err()
        {
            let _ = std::fs::remove_file(tmp);
        }
    }
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_saved_match_details_without_rewriting_the_cache() {
        let detail = detail_text(
            "对局时长 31 分 0 秒\r\n\r\n红方  英雄 22  5 / 5 / 12  补刀 92  装备 1001 999999",
        );
        assert!(detail.contains("红方 · 英雄 / KDA / 补刀 / 装备"));
        assert!(detail.contains("寒冰射手    5 / 5 / 12    补刀 92"));
        assert!(detail.contains(&REFERENCE.items[&1001]));
        assert!(detail.contains("未知装备 #999999"));
    }
}
