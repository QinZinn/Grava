# Template for a Fedora RPM, meant for COPR. The build needs the crates, which
# COPR cannot download, so a vendor tarball is a second source:
#
#   spectool -g grava.spec                        # fetch Source0
#   tar xf Grava-%{version}.tar.gz && cd Grava-%{version}
#   cargo vendor --locked && tar cJf ../grava-%{version}-vendor.tar.xz vendor && cd ..
#   rpmbuild -bs grava.spec                       # then upload the SRPM to COPR
#
# The default build is pure Rust. For the optional native backends add
# BuildRequires pipewire-devel, alsa-lib-devel and clang-devel and put
# --features pipewire,alsa in the %%cargo_build and %%cargo_test lines.

Name:           grava
Version:        0.1.0
Release:        1%{?dist}
Summary:        Terminal audio visualizer that draws Chladni figures with sand

# grava is MIT. The rest is the licenses of the crates compiled into the binary,
# as listed in LICENSE.dependencies (produced in %%build); re-check the
# expression when the dependencies change.
License:        MIT AND (Apache-2.0 OR MIT) AND (Unlicense OR MIT) AND (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT)
URL:            https://github.com/QinZinn/Grava
Source0:        %{url}/archive/refs/tags/v%{version}/Grava-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo-rpm-macros >= 26
BuildRequires:  gcc

%description
grava listens to what the computer plays and draws it in the terminal as sand
on a vibrating plate or drum head. Grains are thrown off the parts of the
surface that move and settle on the nodal lines, so every note makes its own
Chladni figure. It records the monitor of the default output through
PulseAudio or PipeWire and needs no configuration.

%prep
%autosetup -n Grava-%{version} -p1 -a1
%cargo_prep -v vendor

%build
%cargo_build
%{cargo_license_summary}
%{cargo_license} > LICENSE.dependencies

%install
# Only the binary is packaged: %%cargo_install would also install the crate
# sources under /usr/share/cargo/registry, which this package does not ship.
install -Dpm 0755 target/rpm/grava %{buildroot}%{_bindir}/grava
./target/rpm/grava --man | install -Dpm 0644 /dev/stdin %{buildroot}%{_mandir}/man1/grava.1
./target/rpm/grava --completions bash | install -Dpm 0644 /dev/stdin \
    %{buildroot}%{bash_completions_dir}/grava
./target/rpm/grava --completions zsh | install -Dpm 0644 /dev/stdin \
    %{buildroot}%{zsh_completions_dir}/_grava
./target/rpm/grava --completions fish | install -Dpm 0644 /dev/stdin \
    %{buildroot}%{fish_completions_dir}/grava.fish
install -Dpm 0644 assets/config.toml %{buildroot}%{_docdir}/%{name}/config.toml

%check
%cargo_test

%files
%license LICENSE
%license LICENSE.dependencies
%doc README.md
%doc %{_docdir}/%{name}/config.toml
%{_bindir}/grava
%{_mandir}/man1/grava.1*
%{bash_completions_dir}/grava
%{zsh_completions_dir}/_grava
%{fish_completions_dir}/grava.fish

%changelog
* Sat Oct 10 2026 Your Name <you@example.org> - 0.1.0-1
- Initial package
