// Built-in profile for web browsers: camera for video calls, security keys
// for passkeys, a shared Downloads directory, and web links handled by the
// browser itself.
local psbox = import 'psbox';

{
  devices+: { camera: true, 'security-key': true },
  mount+: {
    downloads: psbox.mount.readWrite(psbox.path([psbox.home, 'Downloads']), create=true),
  },
  links+: { http: 'sandbox', https: 'sandbox' },
}
