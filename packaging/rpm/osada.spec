# osada's RPM (EL9, EL10, Fedora). The binary is built beforehand (packaging/build-repo.sh, on EL9:
# glibc 2.34, so the same one runs everywhere); Source0 has it with osada-setup and the docs.
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
# not Recommends: a firewalld pulled in is enabled by the preset and would start, closed, at
# the next boot on a VM that had none
Suggests:       firewalld
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
Pulls in Stalwart (the mail and groupware server), the webmail (maildot) and what osada-setup
mail needs to set it all up behind an osada node: Stalwart's name, domain, Let's Encrypt, DKIM,
the webmail signing in with osada, and the mail instance in the cluster; osada then keeps the
mailboxes (admin -> mail).

%prep
%setup -q

%install
install -Dm755 osada %{buildroot}%{_bindir}/osada
install -Dm755 osada-setup %{buildroot}%{_sbindir}/osada-setup
install -Dm644 INSTALL.md %{buildroot}%{_docdir}/osada/INSTALL.md
install -Dm644 README.md %{buildroot}%{_docdir}/osada/README.md
install -Dm644 LICENSE %{buildroot}%{_licensedir}/osada/LICENSE
install -Dm644 mail.txt %{buildroot}%{_docdir}/osada-mail/README
install -Dm644 -t %{buildroot}%{_datadir}/osada maildot-webmail-*.zip

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
%dir %{_datadir}/osada
%{_datadir}/osada/maildot-webmail-*.zip

%changelog
* Tue Nov 24 2026 osada <packages@repo.th3rumbl3.com> - 0.2.0-3
- osada-setup mail sets up mail whole (webmail included); nodes anywhere, named anything; the
  header reads osada // <instance>; open federation; default OUs

* Tue Nov 24 2026 osada <packages@repo.th3rumbl3.com> - 0.2.0-2
- Fedora; osada-setup --firewall also with ufw; firewalld only suggested

* Tue Nov 24 2026 osada <packages@repo.th3rumbl3.com> - 0.2.0-1
- the first packages: osada (the program and osada-setup) and osada-mail (Stalwart)
