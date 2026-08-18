package config

// Name grammar tests.

import "testing"

func TestValidName(t *testing.T) {
	t.Parallel()

	ok := []string{"firefox", "brave-browser", "org.x.Warpinator", "a", "A._-b", "calibre-ebook-edit"}
	for _, name := range ok {
		if !ValidName(name) {
			t.Errorf("ValidName(%q) = false, want true", name)
		}
	}

	bad := []string{"", "-lead", "has space", "slash/name", "colon:name", "ü"}
	for _, name := range bad {
		if ValidName(name) {
			t.Errorf("ValidName(%q) = true, want false", name)
		}
	}
}

func TestParseIsolation(t *testing.T) {
	t.Parallel()

	cases := []struct {
		in      string
		mode    string
		name    string
		wantErr bool
	}{
		{"", IsolationInstance, DefaultInstance, false},
		{"oneshot", IsolationOneshot, "", false},
		{"instance", IsolationInstance, DefaultInstance, false},
		{"instance:work", IsolationInstance, "work", false},
		{"instance:-bad", "", "", true},
		{"foo", "", "", true},
	}
	for _, tc := range cases {
		got, err := ParseIsolation(tc.in)
		if tc.wantErr {
			if err == nil {
				t.Errorf("ParseIsolation(%q) = %v, want error", tc.in, got)
			}
			continue
		}
		if err != nil {
			t.Errorf("ParseIsolation(%q): %v", tc.in, err)
			continue
		}
		if got.Mode != tc.mode || got.Name != tc.name {
			t.Errorf("ParseIsolation(%q) = %+v, want mode=%s name=%s", tc.in, got, tc.mode, tc.name)
		}
	}
}

func TestDefaultIsolationIsInstance(t *testing.T) {
	t.Parallel()

	app := &Application{Name: "app"}
	got, err := app.Isolation()
	if err != nil {
		t.Fatal(err)
	}
	if got.Mode != IsolationInstance || got.Name != DefaultInstance {
		t.Fatalf("default isolation = %+v", got)
	}
	if !got.Instance() {
		t.Fatal("default isolation must be instance")
	}
	if got.String() != IsolationInstance {
		t.Fatalf("String()=%q", got.String())
	}

	oneshot := Isolation{Mode: IsolationOneshot}
	if oneshot.Instance() || oneshot.String() != IsolationOneshot {
		t.Fatalf("oneshot = %+v (%s)", oneshot, oneshot.String())
	}
}

func TestIsolationFromFlagsMutex(t *testing.T) {
	t.Parallel()

	_, _, err := IsolationFromFlags("instance", "work")
	if err == nil {
		t.Fatal("expected mutex error")
	}
}
