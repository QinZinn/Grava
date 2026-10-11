# This upstream binary package is separate from the Fedora/COPR source template.
# The stage already contains a stripped static binary and compressed manual.
# Preserve them exactly instead of running host-specific postprocessing.
%global debug_package %{nil}
%global __os_install_post %{nil}
%global _build_id_links none
%global use_source_date_epoch_as_buildtime 1
# musl static PIE carries DT_GNU_HASH for its own relocator, without an
# interpreter or shared libraries. RPM's ELF scanner otherwise adds a loader
# dependency even though this payload does not need an external loader.
%global __requires_exclude ^rtld[(]GNU_HASH[)]$

Name:           grava
Version:        %{grava_version}
Release:        1
Summary:        Terminal audio visualizer that draws Chladni figures with sand
License:        MIT AND (Apache-2.0 OR MIT) AND (Unlicense OR MIT) AND (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT) AND Unicode-3.0 AND BSD-2-Clause AND BSD-3-Clause AND Zlib
URL:            https://github.com/QinZinn/Grava
Source0:        grava-%{version}-payload.tar.gz
ExclusiveArch:  x86_64 aarch64

# The default PulseAudio-protocol client also reaches PipeWire through its
# PulseAudio compatibility server. Demo and PCM inputs need neither server.
Recommends:     (pipewire-pulseaudio or pulseaudio)

%description
grava animates sand on a vibrating plate in the terminal. Grains settle on
nodal lines to draw Chladni figures that follow the music. This static build
captures desktop audio through PulseAudio or PipeWire's PulseAudio server,
and also accepts raw PCM from stdin or a FIFO. It includes a deterministic
demo, themes, shell completions and a manual page.

%prep
%setup -q -c

%install
mkdir -p "%{buildroot}"
cp -a usr "%{buildroot}/"

%files
%defattr(0644,root,root,0755)
%attr(0755,root,root) /usr/bin/grava
/usr/share/man/man1/grava.1.gz
/usr/share/bash-completion/completions/grava
/usr/share/zsh/site-functions/_grava
/usr/share/fish/vendor_completions.d/grava.fish
%doc /usr/share/doc/grava
%license /usr/share/licenses/grava
