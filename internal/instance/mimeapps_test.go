package instance

// Host-URL mimeapps tests.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestMergeMimeappsCreatesDefaults(t *testing.T) {
	t.Parallel()

	got, changed := mergeMimeappsDefaults("", map[string]string{
		mimeHTTP:  hostURLDesktopID,
		mimeHTTPS: hostURLDesktopID,
	})
	if !changed {
		t.Fatal("empty file should change")
	}
	if !strings.Contains(got, "[Default Applications]\n") {
		t.Fatalf("missing section:\n%s", got)
	}
	if !strings.Contains(got, mimeHTTP+"="+hostURLDesktopID) {
		t.Fatalf("missing http:\n%s", got)
	}
	if !strings.Contains(got, mimeHTTPS+"="+hostURLDesktopID) {
		t.Fatalf("missing https:\n%s", got)
	}
}

func TestMergeMimeappsUpdatesOnlyHTTPSchemes(t *testing.T) {
	t.Parallel()

	in := "[Default Applications]\n" +
		"text/plain=gedit.desktop\n" +
		"x-scheme-handler/http=firefox.desktop\n" +
		"\n" +
		"[Added Associations]\n" +
		"text/plain=vim.desktop;\n"
	got, changed := mergeMimeappsDefaults(in, map[string]string{
		mimeHTTP:  hostURLDesktopID,
		mimeHTTPS: hostURLDesktopID,
	})
	if !changed {
		t.Fatal("expected change")
	}
	if !strings.Contains(got, "text/plain=gedit.desktop") {
		t.Fatalf("lost existing default:\n%s", got)
	}
	if !strings.Contains(got, "[Added Associations]\ntext/plain=vim.desktop;") {
		t.Fatalf("lost added associations:\n%s", got)
	}
	if strings.Contains(got, "firefox.desktop") {
		t.Fatalf("http handler should be replaced:\n%s", got)
	}
	if !strings.Contains(got, mimeHTTP+"="+hostURLDesktopID) {
		t.Fatalf("missing http:\n%s", got)
	}
	if !strings.Contains(got, mimeHTTPS+"="+hostURLDesktopID) {
		t.Fatalf("missing https:\n%s", got)
	}
}

func TestMergeMimeappsNoChangeWhenAlreadySet(t *testing.T) {
	t.Parallel()

	in := "[Default Applications]\n" +
		"x-scheme-handler/http=psbox-open-url.desktop;\n" +
		"x-scheme-handler/https=psbox-open-url.desktop\n"
	_, changed := mergeMimeappsDefaults(in, map[string]string{
		mimeHTTP:  hostURLDesktopID,
		mimeHTTPS: hostURLDesktopID,
	})
	if changed {
		t.Fatal("already-correct file should be left alone")
	}
}

func TestMergeMimeappsAppendsSection(t *testing.T) {
	t.Parallel()

	in := "[Added Associations]\ntext/plain=vim.desktop;\n"
	got, changed := mergeMimeappsDefaults(in, map[string]string{
		mimeHTTP: hostURLDesktopID,
	})
	if !changed {
		t.Fatal("expected change")
	}
	if !strings.Contains(got, "[Added Associations]\ntext/plain=vim.desktop;") {
		t.Fatalf("lost section:\n%s", got)
	}
	idxAdded := strings.Index(got, "[Added Associations]")
	idxDef := strings.Index(got, "[Default Applications]")
	if idxDef < 0 || idxDef < idxAdded {
		t.Fatalf("default section should be appended:\n%s", got)
	}
}

func TestEnsureHostURLHandlerWritesAndMerges(t *testing.T) {
	root := t.TempDir()
	home := filepath.Join(root, "home")
	env := map[string]string{"HOME": home}

	cfg := filepath.Join(home, ".config")
	if err := os.MkdirAll(cfg, 0o755); err != nil {
		t.Fatal(err)
	}
	existing := "[Default Applications]\ntext/plain=gedit.desktop\n"
	if err := os.WriteFile(filepath.Join(cfg, "mimeapps.list"), []byte(existing), 0o644); err != nil {
		t.Fatal(err)
	}

	if err := ensureHostURLHandler(env); err != nil {
		t.Fatal(err)
	}
	desktop := filepath.Join(home, ".local", "share", "applications", hostURLDesktopID)
	body, err := os.ReadFile(desktop)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(body), "xdg-open %u") {
		t.Fatalf("desktop:\n%s", body)
	}

	// overwrite desktop on a second call
	if err := os.WriteFile(desktop, []byte("stale\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := ensureHostURLHandler(env); err != nil {
		t.Fatal(err)
	}
	body, err = os.ReadFile(desktop)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(body), "stale") {
		t.Fatal("desktop file should be overwritten")
	}

	list, err := os.ReadFile(filepath.Join(cfg, "mimeapps.list"))
	if err != nil {
		t.Fatal(err)
	}
	got := string(list)
	if !strings.Contains(got, "text/plain=gedit.desktop") {
		t.Fatalf("lost existing entry:\n%s", got)
	}
	if !strings.Contains(got, mimeHTTP+"="+hostURLDesktopID) {
		t.Fatalf("missing http:\n%s", got)
	}

	before, err := os.ReadFile(filepath.Join(cfg, "mimeapps.list"))
	if err != nil {
		t.Fatal(err)
	}
	if err := ensureHostURLHandler(env); err != nil {
		t.Fatal(err)
	}
	after, err := os.ReadFile(filepath.Join(cfg, "mimeapps.list"))
	if err != nil {
		t.Fatal(err)
	}
	if string(before) != string(after) {
		t.Fatalf("stable mimeapps.list was rewritten:\n%s\n---\n%s", before, after)
	}
}

func TestEnsureHostURLHandlerUsesXDGDirs(t *testing.T) {
	root := t.TempDir()
	data := filepath.Join(root, "data")
	cfg := filepath.Join(root, "config")
	env := map[string]string{
		"XDG_DATA_HOME":   data,
		"XDG_CONFIG_HOME": cfg,
	}
	if err := ensureHostURLHandler(env); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(data, "applications", hostURLDesktopID)); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(cfg, "mimeapps.list")); err != nil {
		t.Fatal(err)
	}
}

func TestEnsureHostURLHandlerRequiresHome(t *testing.T) {
	t.Parallel()
	if err := ensureHostURLHandler(map[string]string{}); err == nil {
		t.Fatal("expected error")
	}
}

func TestStripMimeappsRemovesOnlyOurHandler(t *testing.T) {
	t.Parallel()

	in := "[Default Applications]\n" +
		"text/plain=gedit.desktop\n" +
		"x-scheme-handler/http=psbox-open-url.desktop;\n" +
		"x-scheme-handler/https=psbox-open-url.desktop\n" +
		"x-scheme-handler/mailto=thunderbird.desktop\n"
	got, changed := stripMimeappsDefaults(in, map[string]string{
		mimeHTTP:  hostURLDesktopID,
		mimeHTTPS: hostURLDesktopID,
	})
	if !changed {
		t.Fatal("expected change")
	}
	if strings.Contains(got, hostURLDesktopID) {
		t.Fatalf("our handler remains:\n%s", got)
	}
	if !strings.Contains(got, "text/plain=gedit.desktop") {
		t.Fatalf("lost other default:\n%s", got)
	}
	if !strings.Contains(got, "x-scheme-handler/mailto=thunderbird.desktop") {
		t.Fatalf("lost mailto:\n%s", got)
	}

	same, changed := stripMimeappsDefaults(got, map[string]string{
		mimeHTTP:  hostURLDesktopID,
		mimeHTTPS: hostURLDesktopID,
	})
	if changed || same != got {
		t.Fatalf("second strip should be a no-op:\n%s", same)
	}

	foreign := "[Default Applications]\n" + mimeHTTP + "=firefox.desktop\n"
	kept, changed := stripMimeappsDefaults(foreign, map[string]string{
		mimeHTTP: hostURLDesktopID,
	})
	if changed || kept != foreign {
		t.Fatalf("foreign handler must stay:\n%s", kept)
	}
}

func TestRemoveHostURLHandlerCleansFiles(t *testing.T) {
	root := t.TempDir()
	home := filepath.Join(root, "home")
	env := map[string]string{"HOME": home}
	if err := ensureHostURLHandler(env); err != nil {
		t.Fatal(err)
	}
	if err := removeHostURLHandler(env); err != nil {
		t.Fatal(err)
	}
	desktop := filepath.Join(home, ".local", "share", "applications", hostURLDesktopID)
	if _, err := os.Stat(desktop); !os.IsNotExist(err) {
		t.Fatalf("desktop should be gone: %v", err)
	}
	list, err := os.ReadFile(filepath.Join(home, ".config", "mimeapps.list"))
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(list), hostURLDesktopID) {
		t.Fatalf("mimeapps still claims us:\n%s", list)
	}
}

func TestRemoveHostURLHandlerMissingIsOK(t *testing.T) {
	root := t.TempDir()
	env := map[string]string{"HOME": filepath.Join(root, "home")}
	if err := removeHostURLHandler(env); err != nil {
		t.Fatal(err)
	}
}

func TestQuoteDesktopExec(t *testing.T) {
	t.Parallel()
	if got := quoteDesktopExec("/usr/bin/psboxa"); got != "/usr/bin/psboxa" {
		t.Fatalf("got %q", got)
	}
	if got := quoteDesktopExec("/opt/my dir/psboxa"); got != `"/opt/my dir/psboxa"` {
		t.Fatalf("got %q", got)
	}
}
