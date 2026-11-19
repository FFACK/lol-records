use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{os::windows::process::CommandExt, process::Command, time::Duration};
use ureq::{
    Agent,
    tls::{TlsConfig, TlsProvider},
};

#[derive(Clone, Deserialize, Serialize)]
pub struct Match {
    pub id: u64,
    pub platform: String,
    pub timestamp: i64,
    pub mode: String,
    pub champion: u64,
    pub stats: Option<Stats>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Stats {
    pub kills: u64,
    pub deaths: u64,
    pub assists: u64,
    pub win: bool,
    pub cs: u64,
    pub items: Vec<u64>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Snapshot {
    pub account: String,
    pub name: String,
    pub level: u64,
    pub ranks: Vec<String>,
    pub synced: i64,
    pub matches: Vec<Match>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Participant {
    #[serde(default)]
    pub spells: [Option<u64>; 2],
    #[serde(default)]
    pub runes: [Option<u64>; 2],
    #[serde(default)]
    pub gold: Option<u64>,
    #[serde(default)]
    pub damage: Option<u64>,
    #[serde(default)]
    pub rank: Option<Rank>,
    #[serde(default)]
    pub summoner_name: Option<String>,
    #[serde(default)]
    pub tag_line: Option<String>,
    #[serde(default)]
    pub puuid: Option<String>,
    pub participant_id: Option<u64>,
    pub team_id: Option<u64>,
    pub champion_id: Option<u64>,
    pub win: Option<bool>,
    pub kills: Option<u64>,
    pub deaths: Option<u64>,
    pub assists: Option<u64>,
    pub cs: Option<u64>,
    pub items: [Option<u64>; 7],
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Detail {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub context: Option<String>,
    pub game_id: u64,
    pub duration_seconds: Option<u64>,
    pub teams: Vec<u64>,
    pub participants: Vec<Participant>,
    pub self_id: Option<u64>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub account_puuid: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Rank {
    pub tier: u64,
    pub label: String,
    pub checked_at: i64,
}

impl Detail {
    pub fn score(&self) -> Option<(u64, u64)> {
        if self.teams != [100, 200] || self.participants.len() != 10 {
            return None;
        }
        let sum = |team| {
            let mut count = 0;
            let total = self
                .participants
                .iter()
                .filter(|p| p.team_id == Some(team))
                .try_fold(0u64, |total, p| {
                    count += 1;
                    total.checked_add(p.kills?)
                })?;
            (count == 5).then_some(total)
        };
        Some((sum(100)?, sum(200)?))
    }
}

#[derive(Deserialize, Serialize)]
pub enum Reply {
    Rank {
        puuid: String,
        rank: Option<Rank>,
    },
    Live {
        phase: String,
        detail: Option<Box<Detail>>,
    },
    Snapshot(Box<Snapshot>),
    Detail(Box<Detail>),
    AccessDenied,
    Error(String),
}
#[derive(Clone, Copy)]
pub enum Request {
    Search([u8; 128]),
    Live,
    Rank([u8; 36]),
    Sync,
    Detail(u64),
    Profile([u8; 36]),
}

pub(crate) struct Session {
    port: u16,
    header: String,
    agent: Agent,
}

impl Session {
    pub(crate) fn connect() -> Result<Self, Reply> {
        let system = std::env::var_os("SystemRoot")
            .ok_or_else(|| Reply::Error("无法定位 Windows PowerShell。".into()))?;
        let shell =
            std::path::PathBuf::from(system).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let output = Command::new(shell)
            .args(["-NoProfile", "-NonInteractive", "-Command", r#"
$clients = @(Get-CimInstance Win32_Process -Filter "Name='LeagueClientUx.exe' OR Name='LeagueClient.exe'" | Sort-Object Name -Descending)
if (!$clients.Count) { exit 2 }
foreach ($client in $clients) {
    if ($client.CommandLine -match '--app-port=(\d+)') {
        $lcuPort = $Matches[1]
        if ($client.CommandLine -match '--remoting-auth-token=([^\s"]+)') {
            [Console]::Write($lcuPort + ':' + $Matches[1])
            exit 0
        }
    }
}
if (@($clients | Where-Object { !$_.CommandLine }).Count) { exit 3 }
exit 4
"#])
            .creation_flags(0x08000000)
            .output()
            .map_err(|_| Reply::Error("无法检查正在运行的游戏客户端。".into()))?;
        match output.status.code() {
            Some(2) => return Err(Reply::Error("请先启动英雄联盟并登录到大厅。".into())),
            Some(3) => return Err(Reply::AccessDenied),
            Some(0) => (),
            _ => return Err(Reply::Error("未找到当前 LCU 连接信息。".into())),
        }
        let raw = String::from_utf8_lossy(&output.stdout);
        let (port, token) = raw
            .trim()
            .split_once(':')
            .ok_or_else(|| Reply::Error("无法解析 LCU 连接信息。".into()))?;
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| Reply::Error("LCU 端口无效。".into()))?;
        let header = format!("Basic {}", STANDARD.encode(format!("riot:{token}")));
        let agent = Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(8)))
            // LCU presents a local certificate. This client only contacts loopback.
            .tls_config(
                TlsConfig::builder()
                    .provider(TlsProvider::NativeTls)
                    .disable_verification(true)
                    .build(),
            )
            .build()
            .new_agent();
        Ok(Self {
            port,
            header,
            agent,
        })
    }

    fn get(&self, path: &str) -> Result<Value, Reply> {
        self.get_with_timeout(path, Duration::from_secs(8))
    }

    fn get_with_timeout(&self, path: &str, timeout: Duration) -> Result<Value, Reply> {
        self.agent
            .get(format!("https://127.0.0.1:{}{path}", self.port))
            .header("Authorization", &self.header)
            .config()
            .timeout_global(Some(timeout))
            .build()
            .call()
            .and_then(|mut response| response.body_mut().read_json())
            .map_err(|error| match error {
                ureq::Error::StatusCode(code) => {
                    Reply::Error(format!("LCU 返回 HTTP {code}。请确认客户端已登录。"))
                }
                _ => Reply::Error("LCU 未返回有效数据。请确认客户端已登录后重试。".into()),
            })
    }

    fn sync(&self) -> Result<Snapshot, Reply> {
        let account = self.get("/lol-summoner/v1/current-summoner")?;
        self.profile_snapshot(account)
    }

    fn profile(&self, puuid: [u8; 36]) -> Result<Snapshot, Reply> {
        let puuid =
            std::str::from_utf8(&puuid).map_err(|_| Reply::Error("玩家标识无效。".into()))?;
        if !valid_puuid(puuid) {
            return Err(Reply::Error("玩家标识无效。".into()));
        }
        let account = self.get(&format!("/lol-summoner/v2/summoners/puuid/{puuid}"))?;
        self.profile_snapshot(account)
    }

    fn profile_snapshot(&self, account: Value) -> Result<Snapshot, Reply> {
        let puuid = account["puuid"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            .ok_or_else(|| Reply::Error("客户端尚未返回有效账号。".into()))?;
        let (rank, history) = std::thread::scope(|scope| {
            let rank = scope.spawn(|| {
                self.get_with_timeout(
                    &format!("/lol-ranked/v1/ranked-stats/{puuid}"),
                    Duration::from_secs(2),
                )
            });
            let history = self.get(&format!(
                "/lol-match-history/v1/products/lol/{puuid}/matches?begIndex=0&endIndex=19"
            ));
            (
                rank.join().ok().and_then(Result::ok).unwrap_or(Value::Null),
                history,
            )
        });
        let history = history.map_err(|_| {
            Reply::Error(
                "该玩家近期战绩暂不可用，可能受接口权限、隐私设置或客户端状态限制。".into(),
            )
        })?;
        let games = history["games"]["games"]
            .as_array()
            .ok_or_else(|| Reply::Error("战绩返回格式与当前版本不兼容。".into()))?;
        let matches = games
            .iter()
            .filter_map(|game| {
                let identity = game["participantIdentities"].as_array().and_then(|list| {
                    list.iter().find(|row| {
                        let player = &row["player"];
                        player["puuid"].as_str() == Some(puuid)
                            || (!account["summonerId"].is_null()
                                && player["summonerId"] == account["summonerId"])
                    })
                });
                let participants = game["participants"].as_array();
                let player = participants.and_then(|list| match identity {
                    Some(row) => list
                        .iter()
                        .find(|item| item["participantId"] == row["participantId"]),
                    None if list.len() == 1 => list.first(),
                    None => None,
                });
                Some(Match {
                    id: game["gameId"].as_u64()?,
                    platform: string(&game["platformId"]).into(),
                    timestamp: game["gameCreation"].as_i64().unwrap_or_default(),
                    mode: string(&game["gameMode"]).into(),
                    champion: player.map(|p| number(&p["championId"])).unwrap_or_default(),
                    stats: player.and_then(|p| stats(&p["stats"])),
                })
            })
            .collect();
        let ranks = rank["queues"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| {
                let name = match row["queueType"].as_str()? {
                    "RANKED_SOLO_5x5" => "单双排",
                    "RANKED_FLEX_SR" => "灵活排位",
                    _ => return None,
                };
                let tier = match string(&row["tier"]) {
                    "IRON" => "黑铁",
                    "BRONZE" => "青铜",
                    "SILVER" => "白银",
                    "GOLD" => "黄金",
                    "PLATINUM" => "铂金",
                    "EMERALD" => "翡翠",
                    "DIAMOND" => "钻石",
                    "MASTER" => "大师",
                    "GRANDMASTER" => "宗师",
                    "CHALLENGER" => "王者",
                    _ => "未定级",
                };
                Some(format!(
                    "{name} {tier} {} {} LP",
                    string(&row["division"]),
                    number(&row["leaguePoints"])
                ))
            })
            .collect();
        let game_name = account["gameName"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| account["displayName"].as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("当前召唤师");
        let tagline = string(&account["tagLine"]);
        let name = if tagline.is_empty() {
            game_name.to_owned()
        } else {
            format!("{game_name}#{tagline}")
        };
        let platform = games
            .first()
            .map(|game| string(&game["platformId"]))
            .unwrap_or_default();
        Ok(Snapshot {
            account: format!("{platform}:{puuid}"),
            name,
            level: number(&account["summonerLevel"]),
            ranks,
            synced: chrono::Local::now().timestamp_millis(),
            matches,
        })
    }

    fn detail(&self, id: u64) -> Result<Detail, Reply> {
        let game = self.get(&format!("/lol-match-history/v1/games/{id}"))?;
        if game["gameId"].as_u64().is_some_and(|game_id| game_id != id) {
            return Err(Reply::Error("客户端返回了其他对局。".into()));
        }
        let account = self.get("/lol-summoner/v1/current-summoner").ok();
        parse_detail(id, &game, account.as_ref())
            .ok_or_else(|| Reply::Error("单局详情缺少参赛者数据。".into()))
    }
}

fn parse_detail(id: u64, game: &Value, account: Option<&Value>) -> Option<Detail> {
    let account_puuid = account
        .and_then(|account| account["puuid"].as_str())
        .filter(|value| !value.is_empty());
    let self_id = account.and_then(|account| {
        let mut matches = game["participantIdentities"]
            .as_array()?
            .iter()
            .filter(|row| {
                let player = &row["player"];
                account_puuid.is_some_and(|puuid| player["puuid"].as_str() == Some(puuid))
                    || (account["summonerId"]
                        .as_u64()
                        .is_some_and(|id| id != 0 && player["summonerId"].as_u64() == Some(id)))
            })
            .filter_map(|row| row["participantId"].as_u64());
        let id = matches.next()?;
        matches.next().is_none().then_some(id)
    });
    let participants: Vec<_> = game["participants"]
        .as_array()?
        .iter()
        .filter(|p| p.is_object())
        .map(|p| {
            let stats = &p["stats"];
            let lane = stats["totalMinionsKilled"].as_u64();
            let jungle = stats["neutralMinionsKilled"].as_u64();
            let identity = game["participantIdentities"].as_array().and_then(|rows| {
                rows.iter()
                    .find(|row| row["participantId"] == p["participantId"])
            });
            let player = identity.map(|identity| &identity["player"]);
            let nonempty = |value: Option<&Value>| {
                value
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            };
            Participant {
                spells: [
                    p["spell1Id"].as_u64().filter(|id| *id != 0),
                    p["spell2Id"].as_u64().filter(|id| *id != 0),
                ],
                runes: [
                    stats["perk0"].as_u64().filter(|id| *id != 0),
                    stats["perkSubStyle"].as_u64().filter(|id| *id != 0),
                ],
                gold: stats["goldEarned"].as_u64(),
                damage: stats["totalDamageDealtToChampions"].as_u64(),
                rank: None,
                summoner_name: nonempty(
                    player.and_then(|p| p.get("gameName").or_else(|| p.get("riotIdGameName"))),
                )
                .or_else(|| nonempty(player.and_then(|p| p.get("summonerName")))),
                tag_line: nonempty(
                    player.and_then(|p| p.get("tagLine").or_else(|| p.get("riotIdTagline"))),
                ),
                puuid: nonempty(player.and_then(|p| p.get("puuid"))),
                participant_id: p["participantId"].as_u64(),
                team_id: p["teamId"].as_u64().filter(|id| *id != 0),
                champion_id: p["championId"].as_u64().filter(|id| *id != 0),
                win: stats["win"].as_bool(),
                kills: stats["kills"].as_u64(),
                deaths: stats["deaths"].as_u64(),
                assists: stats["assists"].as_u64(),
                cs: match (lane, jungle) {
                    (Some(lane), Some(jungle)) => lane.checked_add(jungle),
                    (value, None) | (None, value) => value,
                },
                items: std::array::from_fn(|slot| {
                    stats[format!("item{slot}")].as_u64().filter(|id| *id != 0)
                }),
            }
        })
        .collect();
    if participants.is_empty() {
        return None;
    }
    let mut teams: Vec<u64> = game["teams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|team| team["teamId"].as_u64().filter(|id| *id != 0))
        .chain(participants.iter().filter_map(|p| p.team_id))
        .collect();
    teams.sort_unstable();
    teams.dedup();
    let self_id = self_id.filter(|id| {
        participants
            .iter()
            .filter(|p| p.participant_id == Some(*id))
            .count()
            == 1
    });
    Some(Detail {
        schema_version: 2,
        context: None,
        game_id: id,
        duration_seconds: game["gameDuration"].as_u64(),
        teams,
        participants,
        self_id,
        platform: game["platformId"]
            .as_str()
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        account_puuid: account_puuid.map(str::to_owned),
    })
}

pub fn valid_puuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

pub fn search_text(bytes: &[u8; 128]) -> Option<&str> {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    if bytes[end..].iter().any(|b| *b != 0) {
        return None;
    }
    let text = std::str::from_utf8(&bytes[..end]).ok()?.trim();
    let (name, tag) = text.rsplit_once('#')?;
    (!name.trim().is_empty() && !tag.trim().is_empty() && !text.chars().any(char::is_control))
        .then_some(text)
}

pub fn phase_name(phase: &str) -> &str {
    match phase {
        "None" => "未进入游戏",
        "Lobby" => "大厅",
        "Matchmaking" => "匹配中",
        "ReadyCheck" => "等待确认",
        "ChampSelect" => "英雄选择",
        "GameStart" => "载入对局",
        "InProgress" => "对局中",
        "Reconnect" => "等待重连",
        "WaitingForStats" | "PreEndOfGame" | "EndOfGame" => "对局结算",
        _ => "暂不可用",
    }
}

fn encoded(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

impl Session {
    fn search(&self, bytes: [u8; 128]) -> Result<Snapshot, Reply> {
        let text = search_text(&bytes).ok_or_else(|| {
            Reply::Error("请输入完整的 昵称#编号（最多 128 个 UTF-8 字节）。".into())
        })?;
        let (name, tag) = text.rsplit_once('#').unwrap();
        let alias = self.get(&format!(
            "/lol-summoner/v1/alias/lookup?gameName={}&tagLine={}",
            encoded(name.trim()),
            encoded(tag.trim())
        ));
        if let Ok(alias) = alias
            && let Some(id) = alias["puuid"].as_str().filter(|id| valid_puuid(id))
        {
            return self.profile(id.as_bytes().try_into().unwrap());
        }
        let account = self
            .get(&format!(
                "/lol-summoner/v1/summoners?name={}",
                encoded(text)
            ))
            .map_err(|_| {
                Reply::Error("当前大区未查询到该玩家，请检查昵称#编号及客户端登录大区。".into())
            })?;
        if !account["gameName"]
            .as_str()
            .is_some_and(|n| n.eq_ignore_ascii_case(name.trim()))
            || !account["tagLine"]
                .as_str()
                .is_some_and(|t| t.eq_ignore_ascii_case(tag.trim()))
        {
            return Err(Reply::Error(
                "客户端返回的 Riot ID 不匹配，请检查昵称#编号。".into(),
            ));
        }
        self.profile_snapshot(account)
    }

    fn player_rank(&self, bytes: [u8; 36]) -> Option<Rank> {
        let puuid = std::str::from_utf8(&bytes)
            .ok()
            .filter(|s| valid_puuid(s))?;
        let value = self
            .get(&format!("/lol-ranked/v1/ranked-stats/{puuid}"))
            .ok()?;
        parse_rank(&value)
    }

    fn live(&self) -> Result<Reply, Reply> {
        let phase = self
            .get("/lol-gameflow/v1/gameflow-phase")?
            .as_str()
            .unwrap_or("None")
            .to_owned();
        if !matches!(
            phase.as_str(),
            "ChampSelect" | "GameStart" | "InProgress" | "Reconnect"
        ) {
            return Ok(Reply::Live {
                phase,
                detail: None,
            });
        }
        let selecting = phase == "ChampSelect";
        let session = self.get(if selecting {
            "/lol-champ-select/v1/session"
        } else {
            "/lol-gameflow/v1/session"
        })?;
        let account = self.get("/lol-summoner/v1/current-summoner").ok();
        let detail = parse_live(&phase, &session, account.as_ref());
        Ok(Reply::Live {
            phase,
            detail: detail.map(Box::new),
        })
    }
}

fn parse_rank(value: &Value) -> Option<Rank> {
    let queues = value["queues"].as_array()?;
    let row = queues
        .iter()
        .find(|q| q["queueType"] == "RANKED_SOLO_5x5")
        .or_else(|| queues.iter().find(|q| q["queueType"] == "RANKED_FLEX_SR"))?;
    let (tier, name) = match string(&row["tier"]) {
        "IRON" => (1, "黑铁"),
        "BRONZE" => (2, "青铜"),
        "SILVER" => (3, "白银"),
        "GOLD" => (4, "黄金"),
        "PLATINUM" => (5, "铂金"),
        "EMERALD" => (6, "翡翠"),
        "DIAMOND" => (7, "钻石"),
        "MASTER" => (8, "大师"),
        "GRANDMASTER" => (9, "宗师"),
        "CHALLENGER" => (10, "王者"),
        "NONE" | "UNRANKED" | "" => (0, "未定级"),
        _ => return None,
    };
    let queue = if row["queueType"] == "RANKED_SOLO_5x5" {
        "单双排"
    } else {
        "灵活"
    };
    Some(Rank {
        tier,
        label: format!(
            "当前{queue} {name} {} {}LP",
            string(&row["division"]),
            number(&row["leaguePoints"])
        ),
        checked_at: chrono::Local::now().timestamp_millis(),
    })
}

fn parse_live(phase: &str, session: &Value, account: Option<&Value>) -> Option<Detail> {
    let selecting = phase == "ChampSelect";
    let session = if selecting {
        session
    } else {
        session.get("gameData").unwrap_or(session)
    };
    let mut participants = Vec::new();
    let mut identities = Vec::new();
    for (key, team) in if selecting {
        vec![("myTeam", 300)]
    } else {
        vec![("teamOne", 100), ("teamTwo", 200)]
    } {
        for p in session[key].as_array().into_iter().flatten() {
            let id = participants.len() + 1;
            // Never resolve anonymous identities from cell IDs or summoner IDs.
            let public_name = p["gameName"]
                .as_str()
                .or_else(|| p["riotIdGameName"].as_str())
                .or_else(|| p["summonerName"].as_str())
                .is_some_and(|name| {
                    !name.is_empty()
                        && name != "Summoner"
                        && !name.starts_with("Summoner ")
                        && !name.starts_with("召唤师 ")
                });
            let known = public_name
                && p["puuid"].as_str().is_some_and(valid_puuid)
                && !p["isAnonymous"].as_bool().unwrap_or(false)
                && p["gameName"].as_str() != Some("Summoner");
            identities.push(serde_json::json!({"participantId":id,"player": if known { p.clone() } else { serde_json::json!({"gameName":"匿名 / 未提供身份"}) }}));
            participants.push(serde_json::json!({"participantId":id,"teamId":team,"championId":p["championId"],"spell1Id":p["spell1Id"],"spell2Id":p["spell2Id"],"stats":{}}));
        }
    }
    let mut detail = parse_detail(
        0,
        &serde_json::json!({"participants":participants,"participantIdentities":identities}),
        account,
    )?;
    detail.context = Some(if selecting {
        "当前选人 · 队友（仅客户端公开身份）".into()
    } else {
        "当前对局 · 点击玩家查看战绩".into()
    });
    Some(detail)
}
fn string(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}
fn number(value: &Value) -> u64 {
    value.as_u64().unwrap_or_default()
}
fn stats(value: &Value) -> Option<Stats> {
    Some(Stats {
        kills: value["kills"].as_u64()?,
        deaths: number(&value["deaths"]),
        assists: number(&value["assists"]),
        win: value["win"].as_bool()?,
        cs: number(&value["totalMinionsKilled"]) + number(&value["neutralMinionsKilled"]),
        items: (0..7)
            .map(|slot| number(&value[format!("item{slot}")]))
            .filter(|id| *id != 0)
            .collect(),
    })
}
impl Session {
    pub(crate) fn run(&self, request: Request) -> Reply {
        let session = self;
        let result = match request {
            Request::Search(bytes) => session.search(bytes).map(|s| Reply::Snapshot(Box::new(s))),
            Request::Live => session.live(),
            Request::Rank(bytes) => Ok(Reply::Rank {
                puuid: String::from_utf8_lossy(&bytes).into_owned(),
                rank: session.player_rank(bytes),
            }),
            Request::Sync => session
                .sync()
                .map(|snapshot| Reply::Snapshot(Box::new(snapshot))),
            Request::Profile(puuid) => session
                .profile(puuid)
                .map(|snapshot| Reply::Snapshot(Box::new(snapshot))),
            Request::Detail(id) => session
                .detail(id)
                .map(|detail| Reply::Detail(Box::new(detail))),
        };
        match result {
            Ok(reply) | Err(reply) => reply,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_validates_full_identity_and_encodes_reserved_characters() {
        let mut bytes = [0; 128];
        let name = "玩家 &?#12345";
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        assert_eq!(search_text(&bytes), Some(name));
        assert_eq!(encoded("A&B /#"), "A%26B%20%2F%23");
        bytes[0] = 0;
        assert!(search_text(&bytes).is_none());
        let mut missing = [0; 128];
        missing[..4].copy_from_slice(b"name");
        assert!(search_text(&missing).is_none());
        missing[0] = 255;
        assert!(search_text(&missing).is_none());
    }

    #[test]
    fn parses_combat_setup_without_turning_missing_stats_into_zero() {
        let detail=parse_detail(3,&serde_json::json!({"participants":[{"participantId":1,"spell1Id":4,"spell2Id":14,"stats":{"perk0":8005,"perkSubStyle":8100,"goldEarned":0,"totalDamageDealtToChampions":23456}}]}),None).unwrap();
        let p = &detail.participants[0];
        assert_eq!(p.spells, [Some(4), Some(14)]);
        assert_eq!(p.runes, [Some(8005), Some(8100)]);
        assert_eq!(p.gold, Some(0));
        assert_eq!(p.damage, Some(23456));
        assert_eq!(p.kills, None);
    }

    #[test]
    fn live_selection_excludes_opponents_and_anonymous_identifiers() {
        let id = "12345678-1234-1234-1234-123456789abc";
        let data = serde_json::json!({"myTeam":[{"puuid":id,"gameName":"公开","championId":22},{"puuid":id,"gameName":"匿名","isAnonymous":true}],"theirTeam":[{"puuid":id,"gameName":"对手"}]});
        let detail = parse_live("ChampSelect", &data, None).unwrap();
        assert_eq!(detail.participants.len(), 2);
        assert_eq!(detail.participants[0].puuid.as_deref(), Some(id));
        assert_eq!(detail.participants[1].puuid, None);
        assert_eq!(detail.participants[0].kills, None);
        assert_eq!(detail.participants[0].items, [None; 7]);
    }

    #[test]
    fn live_ingame_reads_nested_game_data_and_rank_prefers_solo() {
        let detail=parse_live("InProgress",&serde_json::json!({"gameData":{"teamOne":[{"championId":22}],"teamTwo":[{"championId":1}]}}),None).unwrap();
        assert_eq!(detail.teams, [100, 200]);
        assert_eq!(detail.participants.len(), 2);
        let rank=parse_rank(&serde_json::json!({"queues":[{"queueType":"RANKED_FLEX_SR","tier":"CHALLENGER"},{"queueType":"RANKED_SOLO_5x5","tier":"EMERALD","division":"II","leaguePoints":37}]})).unwrap();
        assert_eq!(rank.tier, 6);
        assert!(rank.label.contains("当前单双排 翡翠 II 37LP"));
        assert!(parse_rank(&serde_json::json!({})).is_none());
    }
    use serde_json::json;

    #[test]
    fn standard_score_requires_all_ten_players_and_known_kills() {
        let players: Vec<_> = (1..=10)
            .map(|id| {
                json!({
                    "participantId": id, "teamId": if id <= 5 { 100 } else { 200 },
                    "championId": 22, "stats": {"kills": 0}
                })
            })
            .collect();
        let mut game = json!({"participants": players});
        assert_eq!(parse_detail(42, &game, None).unwrap().score(), Some((0, 0)));
        game["participants"][0]["stats"]["kills"] = Value::Null;
        assert_eq!(parse_detail(42, &game, None).unwrap().score(), None);
    }

    #[test]
    fn preserves_other_teams_and_optional_stats_and_item_slots() {
        let game = json!({
            "gameDuration": 0,
            "participantIdentities": [{"participantId": 1, "player": {"puuid": "fixture", "gameName": "测试召唤师", "tagLine": "12345"}}],
            "participants": [
                {"participantId": 1, "teamId": 300, "championId": 22,
                    "stats": {"win": false, "deaths": 0, "neutralMinionsKilled": 3, "item2": 1001, "item6": 3340}},
                {"participantId": 2, "teamId": 400}
            ]
        });
        let detail = parse_detail(17, &game, Some(&json!({"puuid": "fixture"}))).unwrap();
        assert_eq!(detail.teams, [300, 400]);
        assert_eq!(detail.self_id, Some(1));
        assert_eq!(detail.score(), None);
        assert_eq!(detail.duration_seconds, Some(0));
        let player = &detail.participants[0];
        assert_eq!(player.summoner_name.as_deref(), Some("测试召唤师"));
        assert_eq!(player.tag_line.as_deref(), Some("12345"));
        assert_eq!(player.puuid.as_deref(), Some("fixture"));
        assert_eq!(player.kills, None);
        assert_eq!(player.deaths, Some(0));
        assert_eq!(player.cs, Some(3));
        assert_eq!(
            player.items,
            [None, None, Some(1001), None, None, None, Some(3340)]
        );
    }
}
