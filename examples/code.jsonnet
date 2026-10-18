// Visual Studio Code with the directories you work in. The URL handler is a
// second app in the same sandbox, so links from the browser open there.
local psbox = import 'psbox';

{
  mount+: {
    projects: psbox.mount.readWrite(psbox.path([psbox.home, 'Projects']), create=true),
    ssh: psbox.mount.readOnly(psbox.path([psbox.home, '.ssh']), optional=true),
    'etc-ssh': psbox.mount.empty('/etc/ssh'),
  },
  app+: {
    code: { run: ['code'], id: 'code-oss', 'source-id': 'code-oss' },
    'code-url-handler': {
      run: ['code', '--open-url'],
      id: 'code-oss-url-handler',
      'source-id': 'code-oss-url-handler',
    },
  },
}
