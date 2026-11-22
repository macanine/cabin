//! Configure guest DNS to use DNS-over-HTTPS through the embedded proxy.
use crate::config::Config;
use crate::proxy;
use crate::util::shell_quote;
use crate::vm::Runtime;
use crate::{Error, Result};
use std::time::Duration;

pub fn ensure(cfg: &Config, runtime: &Runtime) -> Result<()> {
    if !proxy::should_manage(cfg.proxy.as_deref()) {
        return Ok(());
    }
    let unit = format!(
        "[Unit]\nDescription=Cabin DNS over proxy\nAfter=network.target systemd-resolved.service\n[Service]\nExecStart={} dns-serve\nRestart=on-failure\n[Install]\nWantedBy=multi-user.target\n",
        crate::agentd::HELPER_BIN
    );
    let script = format!(
        r#"set -eu
install -d -m 0755 /etc/systemd/resolved.conf.d
printf '%s' {unit} > /etc/systemd/system/vmctl-dns.service.new
mv /etc/systemd/system/vmctl-dns.service.new /etc/systemd/system/vmctl-dns.service
chmod 0644 /etc/systemd/system/vmctl-dns.service
rm -f /usr/local/lib/vmctl/dns.py
printf '[Resolve]\nDNS=127.0.0.1\nFallbackDNS=\n' > /etc/systemd/resolved.conf.d/99-vmctl.conf
systemctl daemon-reload
systemctl enable vmctl-dns.service >/dev/null
systemctl restart vmctl-dns.service
systemctl is-active --quiet systemd-resolved || systemctl start systemd-resolved
interfaces=$(ip -o -4 addr show scope global | awk '{{print $2}}' | sort -u)
test -n "$interfaces"
for iface in $interfaces; do
    resolvectl dns "$iface" 127.0.0.1
    resolvectl default-route "$iface" yes
done
"#,
        unit = shell_quote(&unit),
    );
    let command = format!("sudo -n sh -c {}", shell_quote(&script));
    let (code, output) = runtime.ssh_capture(&command, Duration::from_secs(45));
    if code != 0 {
        return Err(Error::Message(format!(
            "DNS setup failed: {}",
            output.trim()
        )));
    }
    Ok(())
}
