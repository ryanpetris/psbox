package objects

// Desktop rewrite tests.

import (
	"path/filepath"
	"strings"
	"testing"

	"petris.dev/psbox/internal/config"
)

func TestRewriteSameBinaryAndCommand(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	same, err := RewriteDesktop(&config.DesktopEntry{
		Name:        "brave",
		Kind:        config.KindFreedesktopEntry,
		Application: "brave",
		Value: "[Desktop Entry]\n" +
			"Name=Brave\n" +
			"Exec=brave %U\n" +
			"TryExec=brave\n" +
			"DBusActivatable=true\n",
	}, "brave", paths)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(same, "Exec=psbox launch brave -- %U") {
		t.Fatalf("same-binary rewrite: %s", same)
	}
	if strings.Contains(same, "TryExec=") {
		t.Fatal("TryExec should be dropped")
	}
	if !strings.Contains(same, "DBusActivatable=false") {
		t.Fatal("main group must force DBusActivatable=false")
	}

	cmd, err := RewriteDesktop(&config.DesktopEntry{
		Name:        "idea",
		Kind:        config.KindFreedesktopEntry,
		Application: "intellij-idea",
		Value: "[Desktop Entry]\n" +
			"Exec=/usr/bin/idea %f\n",
	}, "intellij-idea", paths)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(cmd, "Exec=psbox launch intellij-idea --command -- /usr/bin/idea %f") {
		t.Fatalf("path exec rewrite: %s", cmd)
	}

	env, err := RewriteDesktop(&config.DesktopEntry{
		Name:        "app",
		Kind:        config.KindFreedesktopEntry,
		Application: "app",
		Value:       "[Desktop Entry]\nExec=env FOO=bar /usr/bin/app %U\n",
	}, "app", paths)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(env, "Exec=psbox launch app --command -- env FOO=bar /usr/bin/app %U") {
		t.Fatalf("env prefix: %s", env)
	}
}

func TestRewriteOverridesAndInject(t *testing.T) {
	t.Parallel()

	body, err := RewriteDesktop(&config.DesktopEntry{
		Name:        "calibre-gui",
		Kind:        config.KindFreedesktopEntry,
		Application: "calibre",
		Value: "[Desktop Entry]\n" +
			"Exec=calibre --detach %U\n",
		Overrides: []config.Override{{Field: "Exec", Pattern: " --detach"}},
		Inject:    []config.Inject{{Value: "Hidden=true"}},
	}, "calibre", testPaths(t))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(body, "Hidden=true") {
		t.Fatalf("missing inject: %s", body)
	}
	if strings.Contains(body, "--detach") {
		t.Fatalf("override should strip --detach: %s", body)
	}
	if !strings.Contains(body, "Exec=psbox launch calibre -- %U") {
		t.Fatalf("exec after override: %s", body)
	}
}

func TestRewriteInsertsDBusActivatable(t *testing.T) {
	t.Parallel()

	body, err := RewriteDesktop(&config.DesktopEntry{
		Name:        "app",
		Kind:        config.KindFreedesktopEntry,
		Application: "app",
		Value:       "[Desktop Entry]\nName=App\nExec=app\n[Desktop Action foo]\nExec=app --foo\n",
	}, "app", testPaths(t))
	if err != nil {
		t.Fatal(err)
	}
	main, action, ok := strings.Cut(body, "[Desktop Action foo]")
	if !ok {
		t.Fatal(body)
	}
	if !strings.Contains(main, "DBusActivatable=false") {
		t.Fatalf("missing in main group: %s", body)
	}
	if strings.Contains(action, "DBusActivatable") {
		t.Fatalf("should not rewrite action group: %s", body)
	}
}

func testPaths(t *testing.T) config.Paths {
	t.Helper()
	home := t.TempDir()
	return config.Paths{
		Home:       home,
		ConfigHome: filepath.Join(home, ".config"),
		DataHome:   filepath.Join(home, ".local", "share"),
		Root:       filepath.Join(home, "Sandbox"),
	}
}
