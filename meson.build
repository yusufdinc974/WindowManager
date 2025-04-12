project(
  'my-compositor',
  'c',
  version: '0.1.0',
  license: 'MIT',
  default_options: [
    'c_std=c11',
    'warning_level=2',
  ],
)
# Add compiler flags
cc = meson.get_compiler('c')
add_project_arguments(
  cc.get_supported_arguments([
    '-Wno-unused-parameter',
    '-Wno-unused-result',
    '-Wno-missing-braces',
    '-Wundef',
    '-Wvla',
  ]),
  language: 'c',
)
# Add the unstable wlroots API flag
add_project_arguments('-DWLR_USE_UNSTABLE', language: 'c')
# Dependencies
wlroots_dep = dependency('wlroots-0.18')
wayland_server = dependency('wayland-server')
wayland_protos = dependency('wayland-protocols')
xkbcommon = dependency('xkbcommon')
pixman = dependency('pixman-1')
math = cc.find_library('m', required: true)
# Protocol generation
wayland_scanner = find_program('wayland-scanner')
wl_protocol_dir = wayland_protos.get_variable(pkgconfig: 'pkgdatadir')
# XDG shell protocol
xdg_shell_xml = join_paths(wl_protocol_dir, 'stable', 'xdg-shell', 'xdg-shell.xml')
xdg_shell_c = custom_target(
  'xdg-shell-protocol.c',
  input: xdg_shell_xml,
  output: 'xdg-shell-protocol.c',
  command: [wayland_scanner, 'private-code', '@INPUT@', '@OUTPUT@'],
)
xdg_shell_h = custom_target(
  'xdg-shell-protocol.h',
  input: xdg_shell_xml,
  output: 'xdg-shell-protocol.h',
  command: [wayland_scanner, 'client-header', '@INPUT@', '@OUTPUT@'],
)
# Protocol files
protocols_src = [
  xdg_shell_c,
  xdg_shell_h,
]
# Source files
src_files = [
  'src/main.c',
  'src/server.c',
  'src/input.c',
  'src/output.c',
  'src/bsp.c',
  'src/window.c',  # Added window.c source
  protocols_src,
]
# Include directories
inc_dirs = [
  include_directories('src'),
  include_directories('.')  # For protocol headers in build directory
]
# Executable
executable(
  'my-compositor',
  src_files,
  include_directories: inc_dirs,
  dependencies: [
    wlroots_dep,
    wayland_server,
    wayland_protos,
    xkbcommon,
    pixman,
    math,
  ],
  install: true,
)
# Installation
install_data(
  'config.example',
  install_dir: join_paths(get_option('datadir'), 'my-compositor'),
)