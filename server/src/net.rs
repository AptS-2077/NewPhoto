use crate::{
    config::Config,
    error::{ApiError, ApiResult},
};
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use url::Url;

pub fn normalize_base(value: &str) -> ApiResult<String> {
    let mut url = Url::parse(value).map_err(|_| ApiError::validation("服务地址不是有效 URL"))?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiError::validation("服务地址不允许凭据、查询或片段"));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url.to_string())
}
pub fn join(base: &str, path: &str) -> ApiResult<Url> {
    let lowered = path.to_ascii_lowercase();
    if path.is_empty()
        || path.starts_with('/')
        || path.contains([':', '\\', '?', '#'])
        || lowered.contains("%2e")
        || lowered.contains("%2f")
        || lowered.contains("%5c")
        || path.split('/').any(|part| part == "." || part == "..")
    {
        return Err(ApiError::validation("入口路径必须是无穿越的相对路径"));
    }
    Url::parse(&normalize_base(base)?)
        .and_then(|base| base.join(path))
        .map_err(|_| ApiError::validation("入口路径无效"))
}
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(address) => {
            let octets = address.octets();
            !(address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_broadcast()
                || address.is_documentation()
                || address.is_unspecified()
                || address.is_multicast()
                || octets[0] == 0
                || octets[0] >= 240
                || octets[0] == 100 && (64..=127).contains(&octets[1])
                || octets[0] == 198 && (18..=19).contains(&octets[1])
                || octets[0] == 192 && octets[1] == 0 && octets[2] == 0
                || octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
        }
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(mapped));
            }
            let segments = address.segments();
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && segments[1] == 0xdb8
                    || segments[0] == 0x2002
                    || segments[0] == 0x3fff && segments[1] & 0xf000 == 0)
        }
    }
}
pub async fn client(
    config: &Config,
    url: &Url,
    download_origin: Option<&Url>,
) -> ApiResult<reqwest::Client> {
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiError::validation("上游 URL 不符合安全策略"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| ApiError::validation("上游缺少主机名"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| ApiError::validation("上游端口无效"))?;
    let authority = format!("{}:{port}", host.to_ascii_lowercase());
    let explicitly_allowed = config.allowed_hosts.contains(&authority);
    if url.scheme() != "https" && !explicitly_allowed {
        return Err(ApiError::validation(
            "上游默认要求 HTTPS；内网服务须由服务器明确放行 host:port",
        ));
    }
    if let Some(origin) = download_origin {
        if (origin.host_str() != Some(host) || origin.port_or_known_default() != Some(port))
            && !config.result_hosts.contains(&authority)
        {
            return Err(ApiError::validation("成果下载域名未获服务器授权"));
        }
    }
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.trim_matches(['[', ']']), port))
        .await
        .map_err(|_| ApiError::validation("DNS 解析失败"))?
        .collect();
    if addresses.is_empty()
        || (!explicitly_allowed && addresses.iter().any(|address| !public_ip(address.ip())))
    {
        return Err(ApiError::validation("拒绝环回、私网、元数据或保留 IP"));
    }
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .resolve_to_addrs(host, &addresses)
        .build()
        .map_err(|_| ApiError::validation("无法创建受控 HTTP 客户端"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn joins_prefix_once() {
        assert_eq!(
            join("https://example.com/v1/", "images/generations")
                .unwrap()
                .as_str(),
            "https://example.com/v1/images/generations"
        );
        assert_eq!(
            join("https://example.com/v1", "images/edits")
                .unwrap()
                .path(),
            "/v1/images/edits"
        );
    }
    #[test]
    fn rejects_paths() {
        for path in [
            "/images",
            "../x",
            "https://evil/x",
            "%2e%2e/x",
            "x?url=y",
            "x#y",
            "x\\y",
        ] {
            assert!(join("https://example.com/v1/", path).is_err(), "{path}");
        }
    }
    #[test]
    fn rejects_private_ips() {
        use std::net::{Ipv4Addr, Ipv6Addr};
        for octets in [
            [127, 0, 0, 1],
            [10, 9, 8, 4],
            [169, 254, 169, 254],
            [100, 64, 0, 1],
            [192, 0, 0, 8],
            [192, 88, 99, 1],
        ] {
            assert!(!public_ip(IpAddr::V4(Ipv4Addr::from(octets))));
        }
        for address in [
            "::1",
            "fc00::1",
            "fec0::1",
            "::ffff:127.0.0.1",
            "2002:7f00:1::",
        ] {
            assert!(!public_ip(address.parse().unwrap()));
        }
        assert!(public_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(public_ip(IpAddr::V6(Ipv6Addr::new(
            0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111
        ))));
    }
}
