use crate::{
    config::profiles,
    utils::{
        dirs, help,
        network::{NetworkManager, ProxyType},
        tmpl,
    },
};
use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use reqwest_dav::re_exports::url::form_urlencoded;
use serde::{Deserialize, Serialize};
use serde_yaml_ng::Mapping;
use smartstring::alias::String;
use std::collections::HashMap;
use std::string::String as StdString;
use std::time::Duration;
use tauri::Url;
use tokio::fs;

pub(super) fn normalize_profile_home_url(raw: &str) -> Option<String> {
    let url = Url::parse(raw.trim()).ok()?;

    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }

    url.host_str()?;
    Some(url.to_string().into())
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PrfItem {
    pub uid: Option<String>,

    /// enum value: remote | local | script | merge
    #[serde(rename = "type")]
    pub itype: Option<String>,

    pub name: Option<String>,

    pub file: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<Vec<PrfSelected>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<PrfExtra>,

    pub updated: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub option: Option<PrfOption>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,

    #[serde(skip)]
    pub file_data: Option<String>,
}

#[derive(Default, Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PrfSelected {
    pub name: Option<String>,
    pub now: Option<String>,
}

#[derive(Default, Debug, Clone, Copy, Deserialize, Serialize)]
pub struct PrfExtra {
    pub upload: u64,
    pub download: u64,
    pub total: u64,
    pub expire: u64,
}

#[derive(Default, Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PrfOption {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub with_proxy: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_proxy: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_interval: Option<u64>,

    /// HTTP request timeout in seconds
    /// default is 60 seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,

    /// default is `false`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub danger_accept_invalid_certs: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_auto_update: Option<bool>,

    pub merge: Option<String>,

    pub script: Option<String>,

    pub rules: Option<String>,

    pub proxies: Option<String>,

    pub groups: Option<String>,
}

impl PrfOption {
    pub(crate) fn merge(one: Option<&Self>, other: Option<&Self>) -> Option<Self> {
        match (one, other) {
            (Some(a_ref), Some(b_ref)) => {
                let mut result = a_ref.clone();
                result.user_agent = b_ref.user_agent.clone().or(result.user_agent);
                result.with_proxy = b_ref.with_proxy.or(result.with_proxy);
                result.self_proxy = b_ref.self_proxy.or(result.self_proxy);
                result.danger_accept_invalid_certs =
                    b_ref.danger_accept_invalid_certs.or(result.danger_accept_invalid_certs);
                result.allow_auto_update = b_ref.allow_auto_update.or(result.allow_auto_update);
                result.update_interval = b_ref.update_interval.or(result.update_interval);
                result.merge = b_ref.merge.clone().or(result.merge);
                result.script = b_ref.script.clone().or(result.script);
                result.rules = b_ref.rules.clone().or(result.rules);
                result.proxies = b_ref.proxies.clone().or(result.proxies);
                result.groups = b_ref.groups.clone().or(result.groups);
                result.timeout_seconds = b_ref.timeout_seconds.or(result.timeout_seconds);
                Some(result)
            }
            (Some(a_ref), None) => Some(a_ref.clone()),
            (None, Some(b_ref)) => Some(b_ref.clone()),
            (None, None) => None,
        }
    }
}

impl PrfItem {
    /// Builds an item from a partial value that must include `itype`.
    pub(super) async fn from(item: &Self, file_data: Option<String>) -> Result<Self> {
        let itype = item
            .itype
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("type should not be null"))?;
        match itype.as_str() {
            "remote" => {
                let url = item
                    .url
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("url should not be null"))?;
                let name = item.name.as_ref();
                let desc = item.desc.as_ref();
                let option = item.option.as_ref();
                Self::from_url(url, name, desc, option).await
            }
            "local" => {
                let name = item.name.clone().unwrap_or_else(|| "Local File".into());
                let desc = item.desc.clone().unwrap_or_else(|| "".into());
                let option = item.option.as_ref();
                Self::from_local(name, desc, file_data, option).await
            }
            typ => bail!("invalid profile item type \"{typ}\""),
        }
    }

    async fn from_local(
        name: String,
        desc: String,
        file_data: Option<String>,
        option: Option<&PrfOption>,
    ) -> Result<Self> {
        let uid = help::get_uid("L").into();
        let file = format!("{uid}.yaml").into();
        let opt_ref = option.as_ref();
        let update_interval = opt_ref.and_then(|o| o.update_interval);
        let mut merge = opt_ref.and_then(|o| o.merge.clone());
        let mut script = opt_ref.and_then(|o| o.script.clone());
        let mut rules = opt_ref.and_then(|o| o.rules.clone());
        let mut proxies = opt_ref.and_then(|o| o.proxies.clone());
        let mut groups = opt_ref.and_then(|o| o.groups.clone());

        if merge.is_none() {
            let merge_item = &mut Self::from_merge(None);
            profiles::profiles_append_item_safe(merge_item).await?;
            merge = merge_item.uid.clone();
        }
        if script.is_none() {
            let script_item = &mut Self::from_script(None);
            profiles::profiles_append_item_safe(script_item).await?;
            script = script_item.uid.clone();
        }
        if rules.is_none() {
            let rules_item = &mut Self::from_rules();
            profiles::profiles_append_item_safe(rules_item).await?;
            rules = rules_item.uid.clone();
        }
        if proxies.is_none() {
            let proxies_item = &mut Self::from_proxies();
            profiles::profiles_append_item_safe(proxies_item).await?;
            proxies = proxies_item.uid.clone();
        }
        if groups.is_none() {
            let groups_item = &mut Self::from_groups();
            profiles::profiles_append_item_safe(groups_item).await?;
            groups = groups_item.uid.clone();
        }
        Ok(Self {
            uid: Some(uid),
            itype: Some("local".into()),
            name: Some(name),
            desc: Some(desc),
            file: Some(file),
            url: None,
            selected: None,
            extra: None,
            option: Some(PrfOption {
                update_interval,
                merge,
                script,
                rules,
                proxies,
                groups,
                ..PrfOption::default()
            }),
            home: None,
            updated: Some(chrono::Local::now().timestamp() as usize),
            file_data: Some(file_data.unwrap_or_else(|| tmpl::ITEM_LOCAL.into())),
        })
    }

    pub(crate) async fn from_url(
        url: &str,
        name: Option<&String>,
        desc: Option<&String>,
        option: Option<&PrfOption>,
    ) -> Result<Self> {
        let with_proxy = option.is_some_and(|o| o.with_proxy.unwrap_or(false));
        let self_proxy = option.is_some_and(|o| o.self_proxy.unwrap_or(false));
        let accept_invalid_certs = option.is_some_and(|o| o.danger_accept_invalid_certs.unwrap_or(false));
        let allow_auto_update = Some(allow_auto_update_enabled(option));
        let user_agent = option.and_then(|o| o.user_agent.clone());
        let update_interval = option.and_then(|o| o.update_interval);
        let timeout = option.and_then(|o| o.timeout_seconds).unwrap_or(20);
        let mut merge = option.and_then(|o| o.merge.clone());
        let mut script = option.and_then(|o| o.script.clone());
        let mut rules = option.and_then(|o| o.rules.clone());
        let mut proxies = option.and_then(|o| o.proxies.clone());
        let mut groups = option.and_then(|o| o.groups.clone());

        let proxy_type = if self_proxy {
            ProxyType::Localhost
        } else if with_proxy {
            ProxyType::System
        } else {
            ProxyType::None
        };

        let url = fix_dirty_url(url)?;

        let resp = match NetworkManager::new()
            .get(
                url.as_str(),
                proxy_type,
                Some(timeout),
                user_agent.clone(),
                accept_invalid_certs,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                return Err(e).context("failed to fetch remote profile");
            }
        };

        let status_code = resp.status();
        if !status_code.is_success() {
            bail!("failed to fetch remote profile with status {status_code}")
        }

        let header = resp.headers();

        let extra;
        'extra: {
            for (k, v) in header.iter() {
                let key_lower = k.as_str().to_ascii_lowercase();
                if key_lower
                    .strip_suffix("subscription-userinfo")
                    .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with('-'))
                {
                    let sub_info = v.to_str().unwrap_or("");
                    extra = Some(PrfExtra {
                        upload: help::parse_str(sub_info, "upload").unwrap_or(0),
                        download: help::parse_str(sub_info, "download").unwrap_or(0),
                        total: help::parse_str(sub_info, "total").unwrap_or(0),
                        expire: help::parse_str(sub_info, "expire").unwrap_or(0),
                    });
                    break 'extra;
                }
            }
            extra = None;
        }

        let filename = match header.get("Content-Disposition") {
            Some(value) => {
                let filename = format!("{value:?}");
                let filename = filename.trim_matches('"');
                match help::parse_str::<String>(filename, "filename*") {
                    Some(filename) => {
                        let iter = percent_encoding::percent_decode(filename.as_bytes());
                        let filename = iter.decode_utf8().unwrap_or_default();
                        filename.split("''").last().map(|s| s.into())
                    }
                    None => match help::parse_str::<String>(filename, "filename") {
                        Some(filename) => {
                            let filename = filename.trim_matches('"');
                            Some(filename.into())
                        }
                        None => None,
                    },
                }
            }
            None => {
                Some(crate::utils::help::get_last_part_and_decode(url.as_str()).unwrap_or_else(|| "Remote File".into()))
            }
        };
        let update_interval = match update_interval {
            Some(val) => Some(val),
            None => match header.get("profile-update-interval") {
                Some(value) => match value.to_str().unwrap_or("").parse::<u64>() {
                    Ok(val) => Some(val * 60),
                    Err(_) => None,
                },
                None => None,
            },
        };

        let home = header
            .get("profile-web-page-url")
            .and_then(|value| value.to_str().ok())
            .and_then(normalize_profile_home_url);

        let uid = help::get_uid("R").into();
        let file = format!("{uid}.yaml").into();
        let name = name
            .map(|s| s.to_owned())
            .unwrap_or_else(|| filename.map(|s| s.into()).unwrap_or_else(|| "Remote File".into()));
        let data = resp.text();
        let data = data.trim_start_matches('\u{feff}');

        let converted_yaml = convert_sub_content_to_clash_yaml(data);
        let data = converted_yaml.as_str();

        let yaml = serde_yaml_ng::from_str::<Mapping>(data).context("the remote profile data is invalid yaml")?;

        if !yaml.contains_key("proxies") && !yaml.contains_key("proxy-providers") {
            bail!("profile does not contain `proxies` or `proxy-providers`");
        }

        if merge.is_none() {
            let merge_item = &mut Self::from_merge(None);
            profiles::profiles_append_item_safe(merge_item).await?;
            merge = merge_item.uid.clone();
        }
        if script.is_none() {
            let script_item = &mut Self::from_script(None);
            profiles::profiles_append_item_safe(script_item).await?;
            script = script_item.uid.clone();
        }
        if rules.is_none() {
            let rules_item = &mut Self::from_rules();
            profiles::profiles_append_item_safe(rules_item).await?;
            rules = rules_item.uid.clone();
        }
        if proxies.is_none() {
            let proxies_item = &mut Self::from_proxies();
            profiles::profiles_append_item_safe(proxies_item).await?;
            proxies = proxies_item.uid.clone();
        }
        if groups.is_none() {
            let groups_item = &mut Self::from_groups();
            profiles::profiles_append_item_safe(groups_item).await?;
            groups = groups_item.uid.clone();
        }

        Ok(Self {
            uid: Some(uid),
            itype: Some("remote".into()),
            name: Some(name),
            desc: desc.cloned(),
            file: Some(file),
            url: Some(url.as_str().into()),
            selected: None,
            extra,
            option: Some(PrfOption {
                update_interval,
                merge,
                script,
                rules,
                proxies,
                groups,
                allow_auto_update,
                ..PrfOption::default()
            }),
            home,
            updated: Some(chrono::Local::now().timestamp() as usize),
            file_data: Some(data.into()),
        })
    }

    pub(super) fn from_merge(uid: Option<String>) -> Self {
        let (id, template) = if let Some(uid) = uid {
            (uid, tmpl::ITEM_MERGE.into())
        } else {
            (help::get_uid("m").into(), tmpl::ITEM_MERGE_EMPTY.into())
        };
        let file = format!("{id}.yaml").into();

        Self {
            uid: Some(id),
            itype: Some("merge".into()),
            file: Some(file),
            updated: Some(chrono::Local::now().timestamp() as usize),
            file_data: Some(template),
            ..Default::default()
        }
    }

    pub(super) fn from_script(uid: Option<String>) -> Self {
        let id = if let Some(uid) = uid {
            uid
        } else {
            help::get_uid("s").into()
        };
        let file = format!("{id}.js").into();
        Self {
            uid: Some(id),
            itype: Some("script".into()),
            file: Some(file),
            updated: Some(chrono::Local::now().timestamp() as usize),
            file_data: Some(tmpl::ITEM_SCRIPT.into()),
            ..Default::default()
        }
    }

    fn from_rules() -> Self {
        let uid = help::get_uid("r").into();
        let file = format!("{uid}.yaml").into();

        Self {
            uid: Some(uid),
            itype: Some("rules".into()),
            file: Some(file),
            updated: Some(chrono::Local::now().timestamp() as usize),
            file_data: Some(tmpl::ITEM_RULES.into()),
            ..Default::default()
        }
    }

    fn from_proxies() -> Self {
        let uid = help::get_uid("p").into();
        let file = format!("{uid}.yaml").into();

        Self {
            uid: Some(uid),
            itype: Some("proxies".into()),
            file: Some(file),
            updated: Some(chrono::Local::now().timestamp() as usize),
            file_data: Some(tmpl::ITEM_PROXIES.into()),
            ..Default::default()
        }
    }

    fn from_groups() -> Self {
        let uid = help::get_uid("g").into();
        let file = format!("{uid}.yaml").into();

        Self {
            uid: Some(uid),
            itype: Some("groups".into()),
            file: Some(file),
            updated: Some(chrono::Local::now().timestamp() as usize),
            file_data: Some(tmpl::ITEM_GROUPS.into()),
            ..Default::default()
        }
    }

    pub(crate) async fn read_file(&self) -> Result<String> {
        let file = self
            .file
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("could not find the file"))?;
        let path = dirs::app_profiles_dir()?.join(file.as_str());
        let content = fs::read_to_string(&path)
            .await
            .with_context(|| format!("failed to read the file \"{}\"", path.display()))?;
        Ok(content.into())
    }
}

impl PrfItem {
    pub(crate) fn current_merge(&self) -> Option<&String> {
        self.option.as_ref().and_then(|o| o.merge.as_ref())
    }

    pub(crate) fn current_script(&self) -> Option<&String> {
        self.option.as_ref().and_then(|o| o.script.as_ref())
    }

    pub(crate) fn current_rules(&self) -> Option<&String> {
        self.option.as_ref().and_then(|o| o.rules.as_ref())
    }

    pub(crate) fn current_proxies(&self) -> Option<&String> {
        self.option.as_ref().and_then(|o| o.proxies.as_ref())
    }

    pub(crate) fn current_groups(&self) -> Option<&String> {
        self.option.as_ref().and_then(|o| o.groups.as_ref())
    }
}

fn allow_auto_update_enabled(option: Option<&PrfOption>) -> bool {
    option.and_then(|o| o.allow_auto_update).unwrap_or(true)
}

fn fix_dirty_url(input: &str) -> Result<Url> {
    let mut url = match Url::parse(input) {
        Ok(u) => u,
        Err(e) => {
            return Err(anyhow::anyhow!(
                "failed to parse subscription URL: {:?}, input: {}",
                e,
                help::mask_url(input)
            ));
        }
    };

    if url.query().is_none() && url.path().contains('&') {
        let path = url.path().to_string();

        if let Some((clean_path, dirty_params)) = path.split_once('&') {
            url.set_path(clean_path);

            url.query_pairs_mut()
                .extend_pairs(form_urlencoded::parse(dirty_params.as_bytes()));
        }
    }

    Ok(url)
}

fn safe_base64_decode(input: &str) -> Option<StdString> {
    let mut s = input.trim().replace('-', "+").replace('_', "/");
    let rem = s.len() % 4;
    if rem > 0 {
        s.push_str(&"=".repeat(4 - rem));
    }

    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(s.as_bytes()) {
        if let Ok(res) = StdString::from_utf8(bytes) {
            return Some(res);
        }
    }
    None
}

fn parse_ss_uri(uri: &str) -> Option<serde_json::Value> {
    let without_prefix = uri.strip_prefix("ss://")?;
    let (body, remarks) = match without_prefix.split_once('#') {
        Some((b, r)) => (b, percent_encoding::percent_decode_str(r).decode_utf8_lossy().to_string()),
        None => (without_prefix, StdString::new()),
    };

    let (main_part, query_part) = match body.split_once('?') {
        Some((m, q)) => (m, Some(q)),
        None => (body, None),
    };

    let (method, password, host, port) = if let Some((user_info, host_port)) = main_part.rsplit_once('@') {
        let (m, p) = if let Some((m, p)) = user_info.split_once(':') {
            (m.to_string(), p.to_string())
        } else if let Some(decoded) = safe_base64_decode(user_info) {
            if let Some((m, p)) = decoded.split_once(':') {
                (m.to_string(), p.to_string())
            } else {
                return None;
            }
        } else {
            return None;
        };

        let (h, port_str) = host_port.split_once(':')?;
        let port: u16 = port_str.parse().ok()?;
        (m, p, h.to_string(), port)
    } else if let Some(decoded) = safe_base64_decode(main_part) {
        let (user_info, host_port) = decoded.rsplit_once('@')?;
        let (m, p) = user_info.split_once(':')?;
        let (h, port_str) = host_port.split_once(':')?;
        let port: u16 = port_str.parse().ok()?;
        (m.to_string(), p.to_string(), h.to_string(), port)
    } else {
        return None;
    };

    let name = if !remarks.trim().is_empty() {
        remarks.trim().to_string()
    } else {
        format!("SS-{host}:{port}")
    };

    let mut map = serde_json::Map::new();
    map.insert("name".into(), serde_json::Value::String(name));
    map.insert("type".into(), serde_json::Value::String("ss".into()));
    map.insert("server".into(), serde_json::Value::String(host));
    map.insert("port".into(), serde_json::Value::Number(port.into()));
    map.insert("cipher".into(), serde_json::Value::String(method));
    map.insert("password".into(), serde_json::Value::String(password));
    map.insert("udp".into(), serde_json::Value::Bool(true));

    if let Some(query) = query_part {
        for (k, v) in form_urlencoded::parse(query.as_bytes()) {
            if k == "plugin" {
                let parts: Vec<&str> = v.split(';').collect();
                if let Some(plugin_name) = parts.first() {
                    map.insert("plugin".into(), serde_json::Value::String(plugin_name.to_string()));
                    let mut opts = serde_json::Map::new();
                    for opt in &parts[1..] {
                        if let Some((opt_k, opt_v)) = opt.split_once('=') {
                            opts.insert(opt_k.to_string(), serde_json::Value::String(opt_v.to_string()));
                        } else {
                            opts.insert(opt.to_string(), serde_json::Value::Bool(true));
                        }
                    }
                    map.insert("plugin-opts".into(), serde_json::Value::Object(opts));
                }
            }
        }
    }

    Some(serde_json::Value::Object(map))
}

fn parse_vmess_uri(uri: &str) -> Option<serde_json::Value> {
    let raw = uri.strip_prefix("vmess://")?;
    let decoded = safe_base64_decode(raw)?;
    let val: serde_json::Value = serde_json::from_str(&decoded).ok()?;

    let add = val.get("add")?.as_str()?.to_string();
    let port: u16 = match val.get("port")? {
        serde_json::Value::Number(n) => n.as_u64()? as u16,
        serde_json::Value::String(s) => s.parse().ok()?,
        _ => return None,
    };
    let uuid = val.get("id")?.as_str()?.to_string();
    let aid: u64 = val
        .get("aid")
        .and_then(|v| match v {
            serde_json::Value::Number(n) => n.as_u64(),
            serde_json::Value::String(s) => s.parse().ok(),
            _ => None,
        })
        .unwrap_or(0);

    let ps = val.get("ps").and_then(|v| v.as_str()).unwrap_or("").trim();
    let name = if !ps.is_empty() {
        ps.to_string()
    } else {
        format!("VMess-{add}:{port}")
    };

    let cipher = val.get("scy").and_then(|v| v.as_str()).unwrap_or("auto");
    let net = val.get("net").and_then(|v| v.as_str()).unwrap_or("tcp");

    let mut map = serde_json::Map::new();
    map.insert("name".into(), serde_json::Value::String(name));
    map.insert("type".into(), serde_json::Value::String("vmess".into()));
    map.insert("server".into(), serde_json::Value::String(add));
    map.insert("port".into(), serde_json::Value::Number(port.into()));
    map.insert("uuid".into(), serde_json::Value::String(uuid));
    map.insert("alterId".into(), serde_json::Value::Number(aid.into()));
    map.insert("cipher".into(), serde_json::Value::String(cipher.into()));
    map.insert("udp".into(), serde_json::Value::Bool(true));

    let tls = val.get("tls").and_then(|v| v.as_str()).unwrap_or("");
    if tls == "tls" || tls == "1" || tls == "true" {
        map.insert("tls".into(), serde_json::Value::Bool(true));
        if let Some(sni) = val.get("sni").and_then(|v| v.as_str()) {
            if !sni.is_empty() {
                map.insert("servername".into(), serde_json::Value::String(sni.into()));
            }
        }
    }

    map.insert("network".into(), serde_json::Value::String(net.into()));
    if net == "ws" {
        let mut ws_opts = serde_json::Map::new();
        if let Some(path) = val.get("path").and_then(|v| v.as_str()) {
            if !path.is_empty() {
                ws_opts.insert("path".into(), serde_json::Value::String(path.into()));
            }
        }
        if let Some(host) = val.get("host").and_then(|v| v.as_str()) {
            if !host.is_empty() {
                let mut headers = serde_json::Map::new();
                headers.insert("Host".into(), serde_json::Value::String(host.into()));
                ws_opts.insert("headers".into(), serde_json::Value::Object(headers));
            }
        }
        map.insert("ws-opts".into(), serde_json::Value::Object(ws_opts));
    } else if net == "grpc" {
        let mut grpc_opts = serde_json::Map::new();
        if let Some(path) = val.get("path").and_then(|v| v.as_str()) {
            if !path.is_empty() {
                grpc_opts.insert("grpc-service-name".into(), serde_json::Value::String(path.into()));
            }
        }
        map.insert("grpc-opts".into(), serde_json::Value::Object(grpc_opts));
    }

    Some(serde_json::Value::Object(map))
}

fn parse_vless_uri(uri: &str) -> Option<serde_json::Value> {
    let parsed_url = Url::parse(uri).ok()?;
    let uuid = parsed_url.username().to_string();
    let host = parsed_url.host_str()?.to_string();
    let port = parsed_url.port().unwrap_or(443);
    let remarks = parsed_url
        .fragment()
        .map(|f| percent_encoding::percent_decode_str(f).decode_utf8_lossy().to_string())
        .unwrap_or_default();

    let name = if !remarks.trim().is_empty() {
        remarks.trim().to_string()
    } else {
        format!("VLESS-{host}:{port}")
    };

    let mut map = serde_json::Map::new();
    map.insert("name".into(), serde_json::Value::String(name));
    map.insert("type".into(), serde_json::Value::String("vless".into()));
    map.insert("server".into(), serde_json::Value::String(host));
    map.insert("port".into(), serde_json::Value::Number(port.into()));
    map.insert("uuid".into(), serde_json::Value::String(uuid));
    map.insert("udp".into(), serde_json::Value::Bool(true));

    let mut net = "tcp".to_string();
    let mut ws_path = StdString::new();
    let mut ws_host = StdString::new();
    let mut grpc_service = StdString::new();
    let mut reality_opts = serde_json::Map::new();

    for (k, v) in parsed_url.query_pairs() {
        match k.as_ref() {
            "security" => {
                let sec = v.to_ascii_lowercase();
                if sec == "tls" || sec == "reality" {
                    map.insert("tls".into(), serde_json::Value::Bool(true));
                }
            }
            "sni" => {
                map.insert("servername".into(), serde_json::Value::String(v.to_string()));
            }
            "flow" => {
                map.insert("flow".into(), serde_json::Value::String(v.to_string()));
            }
            "type" => {
                net = v.to_ascii_lowercase();
            }
            "path" => {
                ws_path = v.to_string();
            }
            "host" => {
                ws_host = v.to_string();
            }
            "serviceName" => {
                grpc_service = v.to_string();
            }
            "pbk" => {
                reality_opts.insert("public-key".into(), serde_json::Value::String(v.to_string()));
            }
            "sid" => {
                reality_opts.insert("short-id".into(), serde_json::Value::String(v.to_string()));
            }
            "fp" => {
                map.insert("client-fingerprint".into(), serde_json::Value::String(v.to_string()));
            }
            _ => {}
        }
    }

    if !reality_opts.is_empty() {
        map.insert("reality-opts".into(), serde_json::Value::Object(reality_opts));
    }

    map.insert("network".into(), serde_json::Value::String(net.clone()));
    if net == "ws" {
        let mut ws_opts = serde_json::Map::new();
        if !ws_path.is_empty() {
            ws_opts.insert("path".into(), serde_json::Value::String(ws_path));
        }
        if !ws_host.is_empty() {
            let mut headers = serde_json::Map::new();
            headers.insert("Host".into(), serde_json::Value::String(ws_host));
            ws_opts.insert("headers".into(), serde_json::Value::Object(headers));
        }
        map.insert("ws-opts".into(), serde_json::Value::Object(ws_opts));
    } else if net == "grpc" && !grpc_service.is_empty() {
        let mut grpc_opts = serde_json::Map::new();
        grpc_opts.insert("grpc-service-name".into(), serde_json::Value::String(grpc_service));
        map.insert("grpc-opts".into(), serde_json::Value::Object(grpc_opts));
    }

    Some(serde_json::Value::Object(map))
}

fn parse_trojan_uri(uri: &str) -> Option<serde_json::Value> {
    let parsed_url = Url::parse(uri).ok()?;
    let password = parsed_url.username().to_string();
    let host = parsed_url.host_str()?.to_string();
    let port = parsed_url.port().unwrap_or(443);
    let remarks = parsed_url
        .fragment()
        .map(|f| percent_encoding::percent_decode_str(f).decode_utf8_lossy().to_string())
        .unwrap_or_default();

    let name = if !remarks.trim().is_empty() {
        remarks.trim().to_string()
    } else {
        format!("Trojan-{host}:{port}")
    };

    let mut map = serde_json::Map::new();
    map.insert("name".into(), serde_json::Value::String(name));
    map.insert("type".into(), serde_json::Value::String("trojan".into()));
    map.insert("server".into(), serde_json::Value::String(host));
    map.insert("port".into(), serde_json::Value::Number(port.into()));
    map.insert("password".into(), serde_json::Value::String(password));
    map.insert("udp".into(), serde_json::Value::Bool(true));

    let mut net = "tcp".to_string();
    let mut ws_path = StdString::new();
    let mut ws_host = StdString::new();
    let mut grpc_service = StdString::new();

    for (k, v) in parsed_url.query_pairs() {
        match k.as_ref() {
            "sni" => {
                map.insert("sni".into(), serde_json::Value::String(v.to_string()));
            }
            "allowInsecure" | "insecure" => {
                if v == "1" || v == "true" {
                    map.insert("skip-cert-verify".into(), serde_json::Value::Bool(true));
                }
            }
            "type" => {
                net = v.to_ascii_lowercase();
            }
            "path" => {
                ws_path = v.to_string();
            }
            "host" => {
                ws_host = v.to_string();
            }
            "serviceName" => {
                grpc_service = v.to_string();
            }
            _ => {}
        }
    }

    map.insert("network".into(), serde_json::Value::String(net.clone()));
    if net == "ws" {
        let mut ws_opts = serde_json::Map::new();
        if !ws_path.is_empty() {
            ws_opts.insert("path".into(), serde_json::Value::String(ws_path));
        }
        if !ws_host.is_empty() {
            let mut headers = serde_json::Map::new();
            headers.insert("Host".into(), serde_json::Value::String(ws_host));
            ws_opts.insert("headers".into(), serde_json::Value::Object(headers));
        }
        map.insert("ws-opts".into(), serde_json::Value::Object(ws_opts));
    } else if net == "grpc" && !grpc_service.is_empty() {
        let mut grpc_opts = serde_json::Map::new();
        grpc_opts.insert("grpc-service-name".into(), serde_json::Value::String(grpc_service));
        map.insert("grpc-opts".into(), serde_json::Value::Object(grpc_opts));
    }

    Some(serde_json::Value::Object(map))
}

fn parse_hysteria2_uri(uri: &str) -> Option<serde_json::Value> {
    let normalized = if uri.starts_with("hy2://") {
        format!("hysteria2://{}", &uri[6..])
    } else {
        uri.to_string()
    };
    let parsed_url = Url::parse(&normalized).ok()?;
    let password = parsed_url.username().to_string();
    let host = parsed_url.host_str()?.to_string();
    let port = parsed_url.port().unwrap_or(443);
    let remarks = parsed_url
        .fragment()
        .map(|f| percent_encoding::percent_decode_str(f).decode_utf8_lossy().to_string())
        .unwrap_or_default();

    let name = if !remarks.trim().is_empty() {
        remarks.trim().to_string()
    } else {
        format!("Hy2-{host}:{port}")
    };

    let mut map = serde_json::Map::new();
    map.insert("name".into(), serde_json::Value::String(name));
    map.insert("type".into(), serde_json::Value::String("hysteria2".into()));
    map.insert("server".into(), serde_json::Value::String(host));
    map.insert("port".into(), serde_json::Value::Number(port.into()));
    map.insert("password".into(), serde_json::Value::String(password));

    for (k, v) in parsed_url.query_pairs() {
        match k.as_ref() {
            "sni" => {
                map.insert("sni".into(), serde_json::Value::String(v.to_string()));
            }
            "insecure" => {
                if v == "1" || v == "true" {
                    map.insert("skip-cert-verify".into(), serde_json::Value::Bool(true));
                }
            }
            "obfs" => {
                map.insert("obfs".into(), serde_json::Value::String(v.to_string()));
            }
            "obfs-password" => {
                map.insert("obfs-password".into(), serde_json::Value::String(v.to_string()));
            }
            _ => {}
        }
    }

    Some(serde_json::Value::Object(map))
}

fn parse_json_node_array(nodes: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut proxies = Vec::new();
    for (idx, item) in nodes.iter().enumerate() {
        let obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };
        let server = match obj.get("server").and_then(|v| v.as_str()) {
            Some(s) if s != "8.8.8.8" => s,
            _ => continue,
        };
        let port: u16 = match obj.get("server_port").or_else(|| obj.get("port")) {
            Some(serde_json::Value::Number(n)) => match n.as_u64() {
                Some(p) => p as u16,
                None => continue,
            },
            Some(serde_json::Value::String(s)) => match s.parse().ok() {
                Some(p) => p,
                None => continue,
            },
            _ => continue,
        };
        let password = match obj.get("password").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => continue,
        };
        let cipher = match obj.get("method").or_else(|| obj.get("cipher")).and_then(|v| v.as_str()) {
            Some(c) => c,
            None => continue,
        };

        let remarks = obj.get("remarks").and_then(|v| v.as_str()).unwrap_or("").trim();
        if remarks == "Surinameas" {
            continue;
        }

        let name = if !remarks.is_empty() {
            remarks.to_string()
        } else {
            format!("Node-{}-{}", idx + 1, server)
        };

        let mut proxy = serde_json::Map::new();
        proxy.insert("name".into(), serde_json::Value::String(name));
        proxy.insert("type".into(), serde_json::Value::String("ss".into()));
        proxy.insert("server".into(), serde_json::Value::String(server.to_string()));
        proxy.insert("port".into(), serde_json::Value::Number(port.into()));
        proxy.insert("cipher".into(), serde_json::Value::String(cipher.to_string()));
        proxy.insert("password".into(), serde_json::Value::String(password.to_string()));
        proxy.insert("udp".into(), serde_json::Value::Bool(true));

        proxies.push(serde_json::Value::Object(proxy));
    }
    proxies
}

fn convert_sub_content_to_clash_yaml(raw_data: &str) -> StdString {
    let trimmed = raw_data.trim();

    if (trimmed.contains("proxies:") || trimmed.contains("proxy-providers:") || trimmed.contains("proxy-groups:"))
        && serde_yaml_ng::from_str::<Mapping>(trimmed).is_ok()
    {
        return trimmed.to_string();
    }

    let mut proxies: Vec<serde_json::Value> = Vec::new();

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(arr) = val.as_array() {
            proxies.extend(parse_json_node_array(arr));
        } else if let Some(obj) = val.as_object() {
            if let Some(arr) = obj.get("servers").or_else(|| obj.get("proxies")).and_then(|v| v.as_array()) {
                proxies.extend(parse_json_node_array(arr));
            }
        }
    }

    let mut candidate_str = trimmed.to_string();
    if proxies.is_empty() {
        if let Some(decoded) = safe_base64_decode(trimmed) {
            if decoded.contains("://") || decoded.trim_start().starts_with('[') || decoded.trim_start().starts_with('{') {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&decoded) {
                    if let Some(arr) = val.as_array() {
                        proxies.extend(parse_json_node_array(arr));
                    }
                }
                candidate_str = decoded;
            }
        }
    }

    if proxies.is_empty() {
        for line in candidate_str.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with("ss://") {
                if let Some(p) = parse_ss_uri(line) {
                    proxies.push(p);
                }
            } else if line.starts_with("vmess://") {
                if let Some(p) = parse_vmess_uri(line) {
                    proxies.push(p);
                }
            } else if line.starts_with("vless://") {
                if let Some(p) = parse_vless_uri(line) {
                    proxies.push(p);
                }
            } else if line.starts_with("trojan://") {
                if let Some(p) = parse_trojan_uri(line) {
                    proxies.push(p);
                }
            } else if line.starts_with("hysteria2://") || line.starts_with("hy2://") {
                if let Some(p) = parse_hysteria2_uri(line) {
                    proxies.push(p);
                }
            }
        }
    }

    if proxies.is_empty() {
        return trimmed.to_string();
    }

    let mut used_names = HashMap::new();
    let mut proxy_names = Vec::new();

    for p in &mut proxies {
        if let Some(obj) = p.as_object_mut() {
            let base_name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("Proxy").to_string();
            let count = used_names.entry(base_name.clone()).or_insert(0);
            *count += 1;
            let final_name = if *count > 1 {
                format!("{base_name} ({count})")
            } else {
                base_name
            };
            proxy_names.push(final_name.clone());
            obj.insert("name".into(), serde_json::Value::String(final_name));
        }
    }

    let mut root = serde_json::Map::new();
    root.insert("port".into(), serde_json::Value::Number(7890.into()));
    root.insert("socks-port".into(), serde_json::Value::Number(7891.into()));
    root.insert("allow-lan".into(), serde_json::Value::Bool(false));
    root.insert("mode".into(), serde_json::Value::String("rule".into()));
    root.insert("log-level".into(), serde_json::Value::String("info".into()));
    root.insert("proxies".into(), serde_json::Value::Array(proxies));

    let mut proxy_group_names: Vec<serde_json::Value> = proxy_names
        .iter()
        .map(|n| serde_json::Value::String(n.clone()))
        .collect();

    if proxy_group_names.is_empty() {
        proxy_group_names.push(serde_json::Value::String("DIRECT".into()));
    }

    let mut proxy_select_proxies = proxy_group_names.clone();
    proxy_select_proxies.push(serde_json::Value::String("DIRECT".into()));

    let mut proxy_group = serde_json::Map::new();
    proxy_group.insert("name".into(), serde_json::Value::String("PROXY".into()));
    proxy_group.insert("type".into(), serde_json::Value::String("select".into()));
    proxy_group.insert("proxies".into(), serde_json::Value::Array(proxy_select_proxies));

    let mut auto_group = serde_json::Map::new();
    auto_group.insert("name".into(), serde_json::Value::String("AUTO-FASTEST".into()));
    auto_group.insert("type".into(), serde_json::Value::String("url-test".into()));
    auto_group.insert("url".into(), serde_json::Value::String("http://www.gstatic.com/generate_204".into()));
    auto_group.insert("interval".into(), serde_json::Value::Number(300.into()));
    auto_group.insert("proxies".into(), serde_json::Value::Array(proxy_group_names));

    let groups = vec![
        serde_json::Value::Object(proxy_group),
        serde_json::Value::Object(auto_group),
    ];
    root.insert("proxy-groups".into(), serde_json::Value::Array(groups));

    let rules = vec![
        serde_json::Value::String("GEOIP,LAN,DIRECT".into()),
        serde_json::Value::String("GEOIP,CN,DIRECT".into()),
        serde_json::Value::String("MATCH,PROXY".into()),
    ];
    root.insert("rules".into(), serde_json::Value::Array(rules));

    match serde_yaml_ng::to_string(&root) {
        Ok(y) => y,
        Err(_) => trimmed.to_string(),
    }
}
