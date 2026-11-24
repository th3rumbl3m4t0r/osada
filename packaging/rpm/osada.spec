# osada's RPM (EL9, EL10). The binary is built beforehand (packaging/build-rpms.sh, on EL9: glibc
# 2.34, so the same one runs on 9 and 10); Source0 has it with osada-setup and the docs.
%global debug_package %{nil}
%global __strip /bin/true

Name:           osada
Version:        %{ver}
Release:        %{rel}%{?dist}
Summary:        Self-hosted files, wiki, sign-in and groupware for a household or an organisation
License:        AGPL-3.0-only
URL:            https://repo.th3rumbl3.com/
Source0:        osada-%{version}.tar.gz
ExclusiveArch:  x86_64

BuildRequires:  systemd-rpm-macros
Requires:       systemd
Requires:       shadow-utils
Requires:       util-linux
Requires:       iproute
Requires:       hostname
Requires:       curl
Recommends:     firewalld
Suggests:       podman

%description
osada: files (with versions, sharing, public links, sync for the apps), a wiki, sign-in for
other programs (OpenID Connect), chat, and mail through Stalwart, on one machine or spread over
several (a first node that keeps the data, edges that answer https and relay to it). osada-setup
makes a node: the first one of a new cluster, an edge, another node, certificates, mail.

%package mail
Summary:        osada's mail: Stalwart, the mail server, for osada-setup mail
Requires:       osada = %{version}-%{release}
Requires:       stalwart >= 0.16
Requires:       python3

%description mail
Pulls in Stalwart (the mail and groupware server) and what osada-setup mail needs to set it up
behind an osada node: its listeners on 127.0.0.1, a credential for the node; osada then keeps
its domains and mailboxes (admin -> mail).

%prep
%setup -q

%install
install -Dm755 osada %{buildroot}%{_bindir}/osada
install -Dm755 osada-setup %{buildroot}%{_sbindir}/osada-setup
install -Dm644 INSTALL.md %{buildroot}%{_docdir}/osada/INSTALL.md
install -Dm644 README.md %{buildroot}%{_docdir}/osada/README.md
install -Dm644 LICENSE %{buildroot}%{_licensedir}/osada/LICENSE
install -Dm644 mail.txt %{buildroot}%{_docdir}/osada-mail/README

%preun
%systemd_preun osada.service osada-web-agent.service osada-node.service osada-node-web-agent.service

%postun
# an upgrade: the services that run restart on the new program
%systemd_postun_with_restart osada.service osada-web-agent.service osada-node.service osada-node-web-agent.service

%files
%license %{_licensedir}/osada/LICENSE
%doc %{_docdir}/osada/INSTALL.md
%doc %{_docdir}/osada/README.md
%{_bindir}/osada
%{_sbindir}/osada-setup

%files mail
%doc %{_docdir}/osada-mail/README

%changelog
* Tue Nov 24 2026 osada <packages@repo.th3rumbl3.com> - 0.2.0-1
- the first packages: osada (the program and osada-setup) and osada-mail (Stalwart)
