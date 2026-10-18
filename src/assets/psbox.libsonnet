// The library `import 'psbox'` returns. psbox calls this function with the
// context of the sandbox being evaluated: its name (null for an unnamed
// sandbox), the host home, and the sandbox root.
function(ctx) {
  local lib = self,

  // The sandbox's name; null for an unnamed sandbox.
  name: ctx.name,
  // The host home directory.
  home: ctx.home,
  // The sandbox root (`$PSBOX_ROOT`, default `~/Sandbox`).
  root: ctx.root,
  // `<root>/<name>`, a named sandbox's default home.
  storage:
    if ctx.name == null then error 'psbox.storage: an unnamed sandbox has no storage'
    else ctx.root + '/' + ctx.name,

  // `<root>/<name>.<type>`, such as `psbox.storageFor('bin')`.
  storageFor(type)::
    local word = std.isString(type) && std.length(type) > 0
                 && std.all([std.member('abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-', c)
                             for c in std.stringChars(type)]);
    if !word then error 'psbox.storageFor: the type is letters, digits, "_", and "-", got ' + std.toString(type)
    else lib.storage + '.' + type,

  // Joins path segments with `/`: repeated slashes collapse, empty and `.`
  // segments are skipped, a trailing slash is dropped, and a leading `/` on
  // a later segment is only a separator. `..` is an error.
  path(segments)::
    if !std.isArray(segments) || std.length(segments) == 0 then
      error 'psbox.path takes a non-empty list of strings'
    else if !std.all([std.isString(s) for s in segments]) then
      error 'psbox.path takes a list of strings'
    else
      local parts = [p for s in segments for p in std.split(s, '/') if p != '' && p != '.'];
      if std.member(parts, '..') then error 'psbox.path: ".." is not allowed in ' + std.toString(segments)
      else (if std.startsWith(segments[0], '/') then '/' else '') + std.join('/', parts),

  // Mounts, as complete entries for `mount`.
  mount:: {
    readWrite(source, destination=null, optional=false, create=false)::
      { source: source, mode: 'read-write', optional: optional, create: create }
      + (if destination == null then {} else { destination: destination }),
    readOnly(source, destination=null, optional=false, create=false)::
      { source: source, mode: 'read-only', optional: optional, create: create }
      + (if destination == null then {} else { destination: destination }),
    overlay(source, destination=null, optional=false, create=false)::
      { source: source, mode: 'overlay', optional: optional, create: create }
      + (if destination == null then {} else { destination: destination }),
    empty(destination):: { destination: destination, mode: 'empty' },
  },

  devices:: {
    // Every device class off.
    none: { gpu: false, audio: false, camera: false, gamepad: false, 'security-key': false, alsa: false, usb: [] },
  },

  // The full default of each section, for writing one out again after a
  // profile removed it: `dbus: psbox.defaults.dbus + { mode: 'host' }`.
  defaults:: {
    sandbox: { 'idle-timeout': 60 },
    home:
      if ctx.name == null then { mode: 'temporary' }
      else { mode: 'path', path: lib.storage, cache: 'temporary' },
    devices: { gpu: true, audio: true, camera: false, gamepad: false, 'security-key': false, alsa: false, usb: [] },
    dbus: { mode: 'portal', talk: [] },
    network: { mode: 'automatic' },
    display: {},
    xwayland: { mode: 'builtin', scale: 'native' },
    links: { http: 'host', https: 'host', mailto: 'host' },
  },

  // Built-in profiles. A file in `~/.config/psbox/profiles/` is imported by
  // its path instead.
  profiles:: {
    browser: import 'psbox/profiles/browser.libsonnet',
  },

  // What psbox puts beneath every config: the sections that are on unless
  // a config sets them to null. psbox fills in the rest of each section.
  base:: {
    sandbox: {},
    home: {},
    devices: {},
    dbus: {},
    network: {},
    display: {},
    links: {},
    environment: {},
    mount: {},
    app: {},
    task: {},
  },

  // A hidden field never reaches psbox, so a config that hides one would
  // silently lose it: an error instead.
  local hidden(value, at) =
    if std.isObject(value) then
      [at + f for f in std.objectFieldsAll(value) if !std.objectHas(value, f)]
      + [p for f in std.objectFields(value) for p in hidden(value[f], at + f + '.')]
    else if std.isArray(value) then
      [p for i in std.range(0, std.length(value) - 1) for p in hidden(value[i], at + i + '.')]
    else [],
  local visible(config) =
    local found = hidden(config, '');
    if found != [] then error 'hidden fields never reach psbox: ' + std.join(', ', found)
    else config,

  // `base` and the layers, in order (profiles, the file, `-c` settings).
  compose(layers):: visible(std.foldl(function(acc, layer) acc + layer, layers, lib.base)),

  // The config a task runs with: the sandbox's, with the task's overrides
  // applied on top.
  forTask(config, task)::
    local overrides = if std.objectHas(config.task[task], 'overrides') then config.task[task].overrides else {};
    visible(config + overrides),
}
