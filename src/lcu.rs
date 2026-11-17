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
#[derive(Deserialize, Serialize)]
pub enum Reply {
    Snapshot(Box<Snapshot>, Vec<(u64, String)>),
    Detail(String),
    AccessDenied,
    Error(String),
}
#[derive(Clone, Copy)]
pub enum Request {
    Sync,
    Detail(u64),
}

struct Session {
    port: u16,
    header: String,
    agent: Agent,
}

impl Session {
    fn connect() -> Result<Self, Reply> {
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
        self.agent
            .get(format!("https://127.0.0.1:{}{path}", self.port))
            .header("Authorization", &self.header)
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
        let puuid = account["puuid"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            .ok_or_else(|| Reply::Error("客户端尚未返回有效账号。".into()))?;
        let rank = self.get(&format!("/lol-ranked/v1/ranked-stats/{puuid}"))?;
        let history = self.get(
            "/lol-match-history/v1/products/lol/current-summoner/matches?begIndex=0&endIndex=19",
        )?;
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

    fn detail(&self, id: u64) -> Result<String, Reply> {
        let game = self.get(&format!("/lol-match-history/v1/games/{id}"))?;
        let players = game["participants"]
            .as_array()
            .ok_or_else(|| Reply::Error("单局详情缺少参赛者数据。".into()))?;
        let mut lines = vec![
            format!(
                "对局时长 {} 分 {} 秒",
                number(&game["gameDuration"]) / 60,
                number(&game["gameDuration"]) % 60
            ),
            String::new(),
        ];
        for player in players {
            let team = number(&player["teamId"]);
            let champion = number(&player["championId"]);
            if let Some(stats) = stats(&player["stats"]) {
                lines.push(format!(
                    "{}  英雄 {}  {} / {} / {}  补刀 {}  装备 {}",
                    if team == 100 { "蓝方" } else { "红方" },
                    champion,
                    stats.kills,
                    stats.deaths,
                    stats.assists,
                    stats.cs,
                    stats
                        .items
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
        }
        Ok(lines.join("\r\n"))
    }
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
pub fn run(request: Request) -> Reply {
    match Session::connect().and_then(|session| match request {
        Request::Sync => session.sync().map(|snapshot| {
            let deadline = std::time::Instant::now() + Duration::from_secs(12);
            let details = snapshot
                .matches
                .iter()
                .take_while(|_| std::time::Instant::now() < deadline)
                .filter_map(|game| session.detail(game.id).ok().map(|detail| (game.id, detail)))
                .collect();
            Reply::Snapshot(Box::new(snapshot), details)
        }),
        Request::Detail(id) => session.detail(id).map(Reply::Detail),
    }) {
        Ok(reply) | Err(reply) => reply,
    }
}
