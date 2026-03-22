use anyhow::Result;
use axum_server::tls_rustls::RustlsConfig;
use rcgen::generate_simple_self_signed;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub async fn build_local_tls_config(host_ip: IpAddr) -> Result<RustlsConfig> {
    let subject_alt_names = local_subject_alt_names(host_ip);
    let certified_key = generate_simple_self_signed(subject_alt_names)?;
    let cert_pem = certified_key.cert.pem().into_bytes();
    let key_pem = certified_key.signing_key.serialize_pem().into_bytes();
    let config = RustlsConfig::from_pem(cert_pem, key_pem).await?;
    Ok(config)
}

fn local_subject_alt_names(host_ip: IpAddr) -> Vec<String> {
    let mut names = vec![
        "localhost".to_string(),
        IpAddr::V4(Ipv4Addr::LOCALHOST).to_string(),
        IpAddr::V6(Ipv6Addr::LOCALHOST).to_string(),
    ];

    let host_ip = host_ip.to_string();
    if !names.contains(&host_ip) {
        names.push(host_ip);
    }

    names
}

#[cfg(test)]
mod tests {
    use super::local_subject_alt_names;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn includes_lan_and_loopback_sans() {
        let names = local_subject_alt_names(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)));
        assert!(names.contains(&"localhost".to_string()));
        assert!(names.contains(&"127.0.0.1".to_string()));
        assert!(names.contains(&"::1".to_string()));
        assert!(names.contains(&"192.168.1.50".to_string()));
    }
}
