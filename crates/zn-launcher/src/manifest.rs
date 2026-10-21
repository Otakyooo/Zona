//! Манифест dev-канала: что качать и куда коннектиться.
//!
//! Манифест лежит ассетом `manifest.json` в rolling prerelease «dev»
//! (его пишет `scripts/publish-dev-build.ps1`). Лаунчер качает манифест,
//! сравнивает версию (semver) с установленной, качает клиентский zip и
//! проверяет sha256. `server_addr` — публичный адрес игрового сервера
//! (playit-туннель бокса): тестер жмёт «Играть» и коннектится сразу.

use crate::error::{Error, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::Duration;

/// Тег rolling-релиза dev-сборок в GitHub Releases.
pub const RELEASE_TAG: &str = "dev";
/// Имя ассета манифеста внутри релиза.
pub const MANIFEST_ASSET: &str = "manifest.json";
/// Канал по умолчанию (пока единственный).
pub const DEFAULT_CHANNEL: &str = "dev";
/// Длина sha256 в hex-символах.
const SHA256_HEX_LEN: usize = 64;
/// Базовый URL GitHub API (параметризован ради тестов).
pub const API_BASE: &str = "https://api.github.com";
/// Слаг репозитория: `ZONA_REPO`/`GITHUB_REPOSITORY` на сборке, иначе дефолт.
const DEFAULT_REPO_SLUG: &str = "Otakyooo/Zona";
/// Сколько попыток GET при ТРАНСПОРТНЫХ сбоях (обрыв, DPI, битый ответ).
const HTTP_ATTEMPTS: u32 = 3;
/// Базовый бэкофф между попытками, мс (удваивается).
const HTTP_BACKOFF_MS: u64 = 500;

/// Единый ureq-агент: таймауты (M6-3) + прокси из окружения.
///
/// Прокси: `HTTPS_PROXY`/`ALL_PROXY`/`HTTP_PROXY` + `NO_PROXY` (плюс реестр
/// Windows, если ureq собран с `win-system-proxy`). Нужен потому, что в
/// сетях со строгим фильтром (DPI/антивирус/корпоративный прокси) Rust-TLS
/// клиент может получать ответ, который не парсится
/// («failed to parse header value») — маршрут через прокси лечит.
static AGENT: OnceLock<ureq::Agent> = OnceLock::new();

/// Общий агент лаунчера (лениво, один на процесс).
pub fn agent() -> &'static ureq::Agent {
    AGENT.get_or_init(|| {
        let mut builder = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(60)));
        builder = builder.proxy(ureq::Proxy::try_from_env());
        builder.build().into()
    })
}

/// Манифест релиза: клиентский zip (+ серверный tar для бокса).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub channel: String,
    pub version: String,
    #[serde(default)]
    pub build: Option<u32>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    /// API-URL ассета клиентского zip.
    pub url: String,
    /// sha256 (hex) клиентского zip.
    pub sha256: String,
    /// Размер zip в байтах.
    pub size: u64,
    /// Публичный адрес сервера `host:port` (playit) — куда коннектить клиент.
    #[serde(default)]
    pub server_addr: Option<String>,
    /// Серверный tar.gz (качает deploy/zona-update.sh на боксе).
    #[serde(default)]
    pub server: Option<ServerAsset>,
    /// Отдельный артефакт лаунчера (M6-8): обновляется независимо от игры.
    /// `None` — обновления лаунчера идут только бандлом из клиентского zip.
    #[serde(default)]
    pub launcher: Option<LauncherAsset>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Серверный артефакт того же релиза (не для лаунчера — для бокса).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerAsset {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// Артефакт лаунчера того же релиза: свой zip и своя версия (M6-8).
/// Позволяет выкатить хотфикс лаунчера без пересборки/перекачки игры.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LauncherAsset {
    /// Версия лаунчера в этом артефакте (semver, например `0.1.42`).
    pub version: String,
    /// API-URL ассета лаунчер-зипа.
    pub url: String,
    /// sha256 (hex) зипа.
    pub sha256: String,
    /// Размер зипа в байтах.
    pub size: u64,
}

impl Manifest {
    /// Версия манифеста как semver (иммиграция «0.1.<run_number>» и т.п.).
    pub fn version_semver(&self) -> Result<Version> {
        Version::parse(&self.version)
            .map_err(|e| Error::Version(format!("version '{}': {e}", self.version)))
    }

    /// Структурная валидность: непустой канал, semver-версия, sha256 из 64
    /// hex, ненулевой размер, https-URL. Канал не сверяем — это
    /// [`Manifest::matches_channel`].
    pub fn validate(&self) -> Result<()> {
        if self.channel.trim().is_empty() {
            return Err(Error::Manifest("пустой channel".into()));
        }
        self.version_semver()?;
        if !is_sha256_hex(&self.sha256) {
            return Err(Error::Manifest(format!(
                "sha256 должен быть 64 hex-символами, получено '{}'",
                self.sha256
            )));
        }
        if self.size == 0 {
            return Err(Error::Manifest("size должен быть > 0".into()));
        }
        if !self.url.starts_with("https://") {
            return Err(Error::Manifest(format!(
                "url должен быть https://, получено '{}'",
                self.url
            )));
        }
        if let Some(server) = &self.server {
            if !is_sha256_hex(&server.sha256) {
                return Err(Error::Manifest(format!(
                    "server.sha256 должен быть 64 hex-символами, получено '{}'",
                    server.sha256
                )));
            }
            if server.size == 0 {
                return Err(Error::Manifest("server.size должен быть > 0".into()));
            }
        }
        if let Some(launcher) = &self.launcher {
            self.launcher_version_semver()?;
            if !launcher.url.starts_with("https://") {
                return Err(Error::Manifest(format!(
                    "launcher.url должен быть https://, получено '{}'",
                    launcher.url
                )));
            }
            if !is_sha256_hex(&launcher.sha256) {
                return Err(Error::Manifest(format!(
                    "launcher.sha256 должен быть 64 hex-символами, получено '{}'",
                    launcher.sha256
                )));
            }
            if launcher.size == 0 {
                return Err(Error::Manifest("launcher.size должен быть > 0".into()));
            }
        }
        Ok(())
    }

    /// Совпадает ли канал манифеста с ожидаемым.
    pub fn matches_channel(&self, channel: &str) -> bool {
        self.channel == channel
    }

    /// Версия лаунчер-ассета как semver (None — блока нет).
    pub fn launcher_version_semver(&self) -> Result<Option<Version>> {
        match &self.launcher {
            None => Ok(None),
            Some(launcher) => Version::parse(&launcher.version).map(Some).map_err(|e| {
                Error::Version(format!("launcher.version '{}': {e}", launcher.version))
            }),
        }
    }

    /// Версия лаунчер-ассета, если она строго новее запущенного (M6-8).
    /// `None` — блока нет или обновляться не надо.
    pub fn pending_launcher_version(&self, running: &Version) -> Result<Option<Version>> {
        Ok(self
            .launcher_version_semver()?
            .filter(|offered| offered > running))
    }

    /// Нужно ли обновление: установленной версии нет или версия манифеста
    /// строго новее установленной (семантическое сравнение).
    pub fn is_update_over(&self, installed: Option<&Version>) -> Result<bool> {
        let current = self.version_semver()?;
        Ok(match installed {
            None => true,
            Some(installed) => current > *installed,
        })
    }
}

/// sha256 в hex: ровно 64 символа [0-9a-fA-F].
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == SHA256_HEX_LEN && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Разбор и валидация манифеста из JSON.
pub fn parse(json: &str) -> Result<Manifest> {
    let mut manifest: Manifest = serde_json::from_str(json)?;
    // Края адреса подчищаем: `server_addr` уезжает в манифест из переменной
    // репозитория, а вставка в UI GitHub нередко приносит `\r\n` —
    // `traded-convenient.tun.ply.gg:11114\r\n` не проходит проверку host:port,
    // и лаунчер молча пускает тестеров в offline вместо игры.
    manifest.server_addr = manifest
        .server_addr
        .take()
        .map(|addr| addr.trim().to_string())
        .filter(|addr| !addr.is_empty());
    manifest.validate()?;
    Ok(manifest)
}

/// Слаг репозитория для API-запросов.
pub fn repo_slug() -> String {
    option_env!("ZONA_REPO")
        .or_else(|| option_env!("GITHUB_REPOSITORY"))
        .unwrap_or(DEFAULT_REPO_SLUG)
        .to_string()
}

/// Базовый URL API: рантайм-оверрide `ZONA_API_BASE` (зеркала, тесты),
/// иначе дефолт. Компиляционный `ZONA_REPO`/`GITHUB_REPOSITORY` — про репо.
pub fn api_base() -> String {
    std::env::var("ZONA_API_BASE").unwrap_or_else(|_| API_BASE.to_string())
}

/// PAT (`Contents: read`), вкомпилированный в бинарник на сборке CI.
/// `None` — собрано без токена (локальная/dev-сборка): приватные ассеты
/// не скачаются, лаунчер честно скажет об этом.
///
/// Токен обрезается по краям: секрет в GitHub Actions нередко попадает
/// со случайным `\n`/пробелом при вставке, и `Bearer <token>\n` —
/// невалидное значение заголовка (`InvalidHeaderValue`), из-за чего
/// запрос падал ещё до подключения к сети ("failed to parse header
/// value" на любом канале, ВНЛ вкл/выкл не помогали).
pub fn release_token() -> Option<&'static str> {
    option_env!("ZONA_RELEASE_TOKEN").map(str::trim)
}

/// Качает и разбирает манифест тега [`RELEASE_TAG`] (API-база — [`api_base`]).
pub fn fetch_manifest(token: Option<&str>) -> Result<Manifest> {
    fetch_manifest_from(&api_base(), token)
}

/// То же, но с произвольным базовым URL API (тесты, зеркала).
///
/// Два хопа: JSON релиза по тегу → API-URL ассета `manifest.json`.
pub fn fetch_manifest_from(api_base: &str, token: Option<&str>) -> Result<Manifest> {
    let base = api_base.trim_end_matches('/');
    let release_url = format!("{base}/repos/{}/releases/tags/{RELEASE_TAG}", repo_slug());
    let release_json = http_get_retried(&release_url, token, "application/vnd.github+json")?;
    let asset_url = manifest_asset_url(&release_json, &release_url)?;
    http_get_retried(&asset_url, token, "application/octet-stream").and_then(|body| parse(&body))
}

/// Достаёт API-URL ассета манифеста из JSON релиза.
fn manifest_asset_url(release_json: &str, release_url: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_str(release_json)?;
    let assets = value
        .get("assets")
        .and_then(|a| a.as_array())
        .ok_or_else(|| Error::Manifest(format!("{release_url}: в ответе нет массива assets")))?;
    for asset in assets {
        if asset.get("name").and_then(|n| n.as_str()) == Some(MANIFEST_ASSET) {
            if let Some(url) = asset.get("url").and_then(|u| u.as_str()) {
                return Ok(url.to_string());
            }
        }
    }
    Err(Error::Manifest(format!(
        "{release_url}: ассет {MANIFEST_ASSET} не найден"
    )))
}

/// Внутренний исход одной попытки GET.
enum HttpOnce {
    /// Тело ответа (2xx).
    Body(String),
    /// Осмысленный HTTP-статус (404/401/…) — ретраить бессмысленно.
    Status(u16),
    /// Транспортная ошибка (обрыв, таймаут, непарсящийся ответ) — ретраить.
    Transport(String),
}

/// GET с опциональным Bearer-токеном; не-2xx — [`HttpOnce::Status`].
fn http_get_once(
    url: &str,
    token: Option<&str>,
    accept: &str,
) -> std::result::Result<HttpOnce, Error> {
    let mut request = agent()
        .get(url)
        .header("Accept", accept)
        .header("User-Agent", "zn-launcher");
    if let Some(token) = token {
        // Края токена подчищаем ещё и здесь: секрет/ENV могли приехать
        // с `\n`, а `HeaderValue` такой байт отвергает — запрос умирал
        // бы на конструкторе заголовка, до любого соединения.
        let token = token.trim();
        if !token.is_empty() {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(error) => return Ok(HttpOnce::Transport(format!("{error}"))),
    };
    let status = response.status();
    if !status.is_success() {
        return Ok(HttpOnce::Status(status.as_u16()));
    }
    let mut response = response;
    response
        .body_mut()
        .read_to_string()
        .map(HttpOnce::Body)
        .map_err(|e| Error::Http(format!("GET {url}: чтение тела: {e}")))
}

/// Транспортные ошибки — ретура с бэкоффом (флаки-сети, DPI, битые ответы);
/// HTTP-статусы — сразу. На финальной транспортной ошибке добавляем
/// подсказку про сеть/антивирус/прокси.
fn http_get_retried(url: &str, token: Option<&str>, accept: &str) -> Result<String> {
    let mut last = String::new();
    for attempt in 1..=HTTP_ATTEMPTS {
        match http_get_once(url, token, accept)? {
            HttpOnce::Body(body) => return Ok(body),
            HttpOnce::Status(code) => {
                return Err(Error::Http(format!("GET {url}: HTTP {code}")));
            }
            HttpOnce::Transport(message) => {
                log::debug!("GET {url}: попытка {attempt}/{HTTP_ATTEMPTS}: {message}");
                last = message;
                if attempt < HTTP_ATTEMPTS {
                    std::thread::sleep(Duration::from_millis(
                        HTTP_BACKOFF_MS * 2u64.pow(attempt - 1),
                    ));
                }
            }
        }
    }
    Err(Error::Http(transport_message(url, &last)))
}

/// Сообщение транспортной ошибки; для непарсящегося ответа — с подсказкой.
fn transport_message(url: &str, message: &str) -> String {
    let hint = if message.contains("failed to parse header value") {
        " (ответ не разобрался — похоже на помехи в сети: антивирус/прокси/DPI; \
         проверьте соединение или задайте переменную https_proxy)"
    } else {
        ""
    };
    format!("GET {url}: {message}{hint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_message_hints_on_unparsable_response() {
        let plain = transport_message("https://x/y", "connection refused");
        assert!(plain.contains("connection refused"));
        assert!(
            !plain.contains("помехи"),
            "для обычной ошибки подсказки не надо: {plain}"
        );
        let garbled = transport_message("https://x/y", "http: failed to parse header value");
        assert!(garbled.contains("не разобрался"), "{garbled}");
        assert!(garbled.contains("https_proxy"), "{garbled}");
    }

    const SHA_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const SHA_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn full_json() -> String {
        format!(
            r#"{{
              "channel": "dev",
              "version": "0.1.42",
              "build": 42,
              "commit": "abc1234",
              "created_at": "2026-10-19T10:00:00Z",
              "url": "https://api.github.com/repos/Otakyooo/Zona/releases/assets/1",
              "sha256": "{SHA_A}",
              "size": 123456,
              "server_addr": "zona.playit.gg:7700",
              "server": {{
                "url": "https://api.github.com/repos/Otakyooo/Zona/releases/assets/2",
                "sha256": "{SHA_B}",
                "size": 654321
              }},
              "launcher": {{
                "version": "0.1.42",
                "url": "https://api.github.com/repos/Otakyooo/Zona/releases/assets/3",
                "sha256": "{SHA_A}",
                "size": 4096
              }},
              "notes": "dev-build abc1234"
            }}"#
        )
    }

    fn minimal_json() -> String {
        format!(
            r#"{{
              "channel": "dev",
              "version": "0.1.1",
              "url": "https://api.github.com/repos/Otakyooo/Zona/releases/assets/9",
              "sha256": "{SHA_A}",
              "size": 7
            }}"#
        )
    }

    #[test]
    fn parse_trims_server_addr() {
        // Переменная репозитория с `\r\n` не должна убивать online-режим:
        // адрес приезжает в манифест грязным, лаунчик обязан подрезать.
        let json = full_json().replace(
            "\"server_addr\": \"zona.playit.gg:7700\"",
            "\"server_addr\": \"zona.playit.gg:7700\\r\\n\"",
        );
        let manifest = parse(&json).unwrap();
        assert_eq!(manifest.server_addr.as_deref(), Some("zona.playit.gg:7700"));
    }

    #[test]
    fn parse_drops_whitespace_only_server_addr() {
        let json = full_json().replace(
            "\"server_addr\": \"zona.playit.gg:7700\"",
            "\"server_addr\": \"   \\n\"",
        );
        let manifest = parse(&json).unwrap();
        assert!(manifest.server_addr.is_none());
    }

    #[test]
    fn parses_full_manifest() {
        let manifest = parse(&full_json()).unwrap();
        assert_eq!(manifest.channel, "dev");
        assert_eq!(manifest.version, "0.1.42");
        assert_eq!(manifest.build, Some(42));
        assert_eq!(manifest.commit.as_deref(), Some("abc1234"));
        assert_eq!(manifest.sha256, SHA_A);
        assert_eq!(manifest.size, 123456);
        assert_eq!(manifest.server_addr.as_deref(), Some("zona.playit.gg:7700"));
        let server = manifest.server.as_ref().unwrap();
        assert_eq!(server.sha256, SHA_B);
        assert_eq!(server.size, 654321);
        assert_eq!(manifest.notes.as_deref(), Some("dev-build abc1234"));
    }

    #[test]
    fn parses_minimal_manifest() {
        let manifest = parse(&minimal_json()).unwrap();
        assert!(manifest.build.is_none());
        assert!(manifest.commit.is_none());
        assert!(manifest.created_at.is_none());
        assert!(manifest.server_addr.is_none());
        assert!(manifest.server.is_none());
        assert!(manifest.launcher.is_none());
        assert!(manifest.notes.is_none());
        assert_eq!(manifest.version_semver().unwrap().to_string(), "0.1.1");
    }

    #[test]
    fn parses_launcher_asset() {
        let manifest = parse(&full_json()).unwrap();
        let launcher = manifest.launcher.as_ref().unwrap();
        assert_eq!(launcher.version, "0.1.42");
        assert_eq!(launcher.size, 4096);
        assert_eq!(launcher.sha256, SHA_A);
        assert_eq!(
            manifest.launcher_version_semver().unwrap().unwrap(),
            Version::parse("0.1.42").unwrap()
        );
    }

    #[test]
    fn pending_launcher_version_covers_all_cases() {
        let manifest = parse(&full_json()).unwrap();
        let older = Version::parse("0.1.41").unwrap();
        let same = Version::parse("0.1.42").unwrap();
        let newer = Version::parse("0.1.43").unwrap();
        // манифест несёт 0.1.42
        assert_eq!(
            manifest.pending_launcher_version(&older).unwrap(),
            Some(Version::parse("0.1.42").unwrap())
        );
        assert_eq!(manifest.pending_launcher_version(&same).unwrap(), None);
        assert_eq!(manifest.pending_launcher_version(&newer).unwrap(), None);
        // без блока launcher обновления нет
        let minimal = parse(&minimal_json()).unwrap();
        assert_eq!(minimal.pending_launcher_version(&older).unwrap(), None);
    }

    #[test]
    fn rejects_bad_launcher_asset() {
        // не-semver версия блока launcher
        let json = full_json().replace(
            "\"version\": \"0.1.42\",\n                \"url\": \"https://api.github.com/repos/Otakyooo/Zona/releases/assets/3\"",
            "\"version\": \"0.1.x\",\n                \"url\": \"https://api.github.com/repos/Otakyooo/Zona/releases/assets/3\"",
        );
        assert!(matches!(parse(&json).unwrap_err(), Error::Version(_)));
        // http-url в launcher
        let json = full_json().replace(
            "\"launcher\": {\n                \"version\": \"0.1.42\",\n                \"url\": \"https://api.github.com",
            "\"launcher\": {\n                \"version\": \"0.1.42\",\n                \"url\": \"http://api.github.com",
        );
        assert!(matches!(parse(&json).unwrap_err(), Error::Manifest(_)));
        // нулевой размер launcher
        let json = full_json().replace(
            &format!("\"sha256\": \"{SHA_A}\",\n                \"size\": 4096"),
            &format!("\"sha256\": \"{SHA_A}\",\n                \"size\": 0"),
        );
        assert!(matches!(parse(&json).unwrap_err(), Error::Manifest(_)));
    }

    #[test]
    fn rejects_bad_sha256() {
        for sha in [
            "",
            "abc",
            &SHA_A[..63],
            &format!("{SHA_A}zz"),
            &format!("{SHA_A}{SHA_A}"),
        ] {
            let json = minimal_json().replace(SHA_A, sha);
            let err = parse(&json).unwrap_err();
            assert!(
                matches!(err, Error::Manifest(_)),
                "ожидали отказ на sha256 '{sha}', получили {err:?}"
            );
        }
    }

    #[test]
    fn rejects_bad_version() {
        for version in ["1.2", "abc", "1.2.3.4", "0.1.x"] {
            let json = minimal_json().replace("\"0.1.1\"", &format!("\"{version}\""));
            let err = parse(&json).unwrap_err();
            assert!(
                matches!(err, Error::Version(_)),
                "ожидали отказ на version '{version}', получили {err:?}"
            );
        }
    }

    #[test]
    fn rejects_http_url_and_zero_size() {
        let json = minimal_json().replace("https://api.github.com", "http://api.github.com");
        assert!(matches!(parse(&json).unwrap_err(), Error::Manifest(_)));
        let json = minimal_json().replace("\"size\": 7", "\"size\": 0");
        assert!(matches!(parse(&json).unwrap_err(), Error::Manifest(_)));
    }

    #[test]
    fn foreign_channel_parses_but_does_not_match() {
        let json = full_json().replace("\"channel\": \"dev\"", "\"channel\": \"release\"");
        let manifest = parse(&json).unwrap();
        assert!(!manifest.matches_channel("dev"));
        assert!(manifest.matches_channel("release"));
    }

    #[test]
    fn is_update_over_covers_all_cases() {
        let manifest = parse(&minimal_json()).unwrap();
        // нет установленной версии — обновляемся всегда
        assert!(manifest.is_update_over(None).unwrap());
        let older = Version::parse("0.1.0").unwrap();
        let same = Version::parse("0.1.1").unwrap();
        let newer = Version::parse("0.1.2").unwrap();
        let newer_minor = Version::parse("0.2.0").unwrap();
        let older_minor = Version::parse("0.0.9").unwrap();
        assert!(manifest.is_update_over(Some(&older)).unwrap());
        assert!(!manifest.is_update_over(Some(&same)).unwrap());
        assert!(!manifest.is_update_over(Some(&newer)).unwrap());
        assert!(!manifest.is_update_over(Some(&newer_minor)).unwrap());
        assert!(manifest.is_update_over(Some(&older_minor)).unwrap());
    }

    #[test]
    fn is_sha256_hex_accepts_case() {
        assert!(is_sha256_hex(SHA_A));
        assert!(is_sha256_hex(&SHA_A.to_uppercase()));
        assert!(!is_sha256_hex(""));
        assert!(!is_sha256_hex(&SHA_A[..63]));
        assert!(!is_sha256_hex(&format!("g{SHA_A}"))); // 'g' не hex
        assert!(!is_sha256_hex(&SHA_A[1..])); // пробел внутри — отсекается длиной
    }

    // ---- fetch_manifest_from по локальному TCP-серверу ----

    fn serve_release(listener: std::net::TcpListener, asset_body: String) {
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let head = read_head(&mut reader);
                let addr = listener.local_addr().unwrap();
                if head.contains("/releases/tags/dev") {
                    let release = format!(
                        r#"{{"assets": [
                          {{"name": "manifest.json", "url": "http://{addr}/assets/manifest.json"}},
                          {{"name": "zona-dev-win-x64.zip", "url": "http://{addr}/assets/zip"}}
                        ]}}"#
                    );
                    respond(stream, "200 OK", "application/json", release.as_bytes());
                } else if head.contains("/assets/manifest.json") {
                    respond(
                        stream,
                        "200 OK",
                        "application/octet-stream",
                        asset_body.as_bytes(),
                    );
                } else {
                    respond(stream, "404 Not Found", "text/plain", b"nope");
                }
            }
        });
    }

    fn read_head(reader: &mut impl std::io::BufRead) -> String {
        let mut head = String::new();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            head.push_str(&line);
        }
        head
    }

    fn respond(mut stream: std::net::TcpStream, status: &str, content_type: &str, body: &[u8]) {
        use std::io::Write;
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
        let _ = stream.flush();
    }

    #[test]
    fn fetch_manifest_over_local_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        serve_release(listener, full_json());
        let manifest = fetch_manifest_from(&format!("http://{addr}"), Some("test-token")).unwrap();
        assert_eq!(manifest.version, "0.1.42");
        assert_eq!(manifest.server_addr.as_deref(), Some("zona.playit.gg:7700"));
    }

    #[test]
    fn fetch_manifest_release_404_is_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let _ = read_head(&mut reader);
                respond(stream, "404 Not Found", "text/plain", b"no release");
            }
        });
        let err = fetch_manifest_from(&format!("http://{addr}"), None).unwrap_err();
        assert!(matches!(err, Error::Http(_)), "получили {err:?}");
    }

    #[test]
    fn fetch_manifest_without_manifest_asset_is_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let _ = read_head(&mut reader);
                let release = r#"{"assets": [{"name": "other.zip", "url": "http://x/y"}]}"#;
                respond(stream, "200 OK", "application/json", release.as_bytes());
            }
        });
        let err = fetch_manifest_from(&format!("http://{addr}"), None).unwrap_err();
        assert!(matches!(err, Error::Manifest(_)), "получили {err:?}");
    }
}
