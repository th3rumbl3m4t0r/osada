# Stalwart as released upstream (the x86_64 gnu build), for osada-mail: into /opt/stalwart as
# upstream's installer and osada-setup mail have it. One package for EL9 and EL10 (glibc 2.28+).
%global debug_package %{nil}
%global __strip /bin/true

Name:           stalwart
Version:        %{ver}
Release:        1.osada
Summary:        Stalwart, the mail and groupware server (JMAP, IMAP, SMTP, CalDAV, CardDAV, WebDAV)
License:        AGPL-3.0-only
URL:            https://stalw.art
# https://github.com/stalwartlabs/stalwart/releases/download/v%{version}/stalwart-x86_64-unknown-linux-gnu.tar.gz
Source0:        stalwart-x86_64-unknown-linux-gnu.tar.gz
ExclusiveArch:  x86_64
Requires(pre):  shadow-utils

%description
Stalwart, an all-in-one mail and groupware server written in Rust, as its authors release it
(the program unchanged). Its sources: https://github.com/stalwartlabs/stalwart/tree/v%{version}.
osada-setup mail configures it (/opt/stalwart/etc) and starts it (stalwart.service).

%prep
%setup -q -c

%install
install -Dm755 stalwart %{buildroot}/opt/stalwart/bin/stalwart
install -d %{buildroot}/opt/stalwart/data %{buildroot}/opt/stalwart/etc %{buildroot}/opt/stalwart/logs

%pre
getent group stalwart >/dev/null || groupadd -r stalwart
getent passwd stalwart >/dev/null || useradd -r -g stalwart -d /opt/stalwart -s /sbin/nologin stalwart
exit 0

%postun
if [ $1 -ge 1 ]; then systemctl try-restart stalwart.service >/dev/null 2>&1 || :; fi

%files
%dir %attr(750,stalwart,stalwart) /opt/stalwart
%dir %attr(755,stalwart,stalwart) /opt/stalwart/bin
%attr(755,stalwart,stalwart) /opt/stalwart/bin/stalwart
%dir %attr(750,stalwart,stalwart) /opt/stalwart/data
%dir %attr(750,stalwart,stalwart) /opt/stalwart/etc
%dir %attr(750,stalwart,stalwart) /opt/stalwart/logs

%changelog
* Tue Nov 24 2026 osada <packages@repo.th3rumbl3.com> - 0.16.27-1.osada
- upstream's 0.16.27, for osada-mail
