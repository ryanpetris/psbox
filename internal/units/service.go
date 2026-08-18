package units

// List and stop instantiated psboxd@ units via the user manager.

import (
	"context"
	"fmt"
	"io"
	"sort"
	"strconv"
	"strings"
	"text/tabwriter"
	"time"

	"petris.dev/psbox/internal/config"
	"petris.dev/psbox/internal/instance"
	"petris.dev/psbox/internal/systemd"
)

const (
	unitPrefix     = "psboxd@"
	serviceSuffix  = ".service"
	socketSuffix   = ".socket"
	missingState   = "-"
	defaultNameSep = "/"
)

// Instance is one sandbox/instance pair seen on the user manager.
type Instance struct {
	Identity string
	Escaped  string
	Service  systemd.Unit
	Socket   systemd.Unit
}

// Service lists and stops psboxd user units.
type Service struct {
	ctl systemd.Control
}

// NewService returns a units service.
func NewService(ctl systemd.Control) *Service {
	return &Service{ctl: ctl}
}

// List writes instance rows to stdout. quiet prints identities only.
func (s *Service) List(ctx context.Context, quiet bool, stdout io.Writer) error {
	items, err := s.instances(ctx)
	if err != nil {
		return err
	}
	if quiet {
		for _, item := range items {
			fmt.Fprintln(stdout, displayIdentity(item.Identity))
		}
		return nil
	}
	tw := tabwriter.NewWriter(stdout, 0, 0, 2, ' ', 0)
	fmt.Fprintln(tw, "IDENTITY\tSERVICE\tSOCKET\tPID\tSINCE\tRESTARTS\tACCEPTED")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%s\t%s\n",
			displayIdentity(item.Identity),
			unitState(item.Service),
			unitState(item.Socket),
			formatPID(item.Service),
			formatSince(item.Service),
			formatCount(item.Service, item.Service.NRestarts),
			formatCount(item.Socket, item.Socket.NAccepted),
		)
	}
	return tw.Flush()
}

// Stop stops the service then the socket for one listed instance.
func (s *Service) Stop(ctx context.Context, name string, stderr io.Writer) error {
	items, err := s.instances(ctx)
	if err != nil {
		return err
	}
	item, err := resolveInstance(name, items)
	if err != nil {
		return err
	}
	return s.stopOne(ctx, item, stderr)
}

// StopAll stops every listed instance. It is not an error if none exist.
func (s *Service) StopAll(ctx context.Context, stderr io.Writer) error {
	items, err := s.instances(ctx)
	if err != nil {
		return err
	}
	var first error
	for _, item := range items {
		if err := s.stopOne(ctx, item, stderr); err != nil && first == nil {
			first = err
		}
	}
	return first
}

func (s *Service) stopOne(ctx context.Context, item Instance, stderr io.Writer) error {
	fmt.Fprintf(stderr, "Stopping %s...\n", displayIdentity(item.Identity))
	sock, svc := instance.UnitNames(item.Escaped)
	var first error
	if err := s.ctl.Stop(ctx, svc); err != nil && first == nil {
		first = err
	}
	if err := s.ctl.Stop(ctx, sock); err != nil && first == nil {
		first = err
	}
	return first
}

func (s *Service) instances(ctx context.Context) ([]Instance, error) {
	units, err := s.ctl.List(ctx, []string{unitPrefix + "*" + serviceSuffix, unitPrefix + "*" + socketSuffix})
	if err != nil {
		return nil, err
	}
	return collectInstances(units)
}

// Identities returns listed identities for completion.
func (s *Service) Identities(ctx context.Context) ([]string, error) {
	items, err := s.instances(ctx)
	if err != nil {
		return nil, err
	}
	out := make([]string, 0, len(items))
	for _, item := range items {
		out = append(out, displayIdentity(item.Identity))
	}
	return out, nil
}

// displayIdentity omits the default instance name. firefox/default is firefox.
func displayIdentity(identity string) string {
	sandbox, inst, err := instance.SplitIdentity(identity)
	if err != nil || inst != config.DefaultInstance {
		return identity
	}
	return sandbox
}

func collectInstances(units []systemd.Unit) ([]Instance, error) {
	byEscaped := map[string]*Instance{}
	for _, u := range units {
		escaped, kind, ok := parsePSBoxUnit(u.Name)
		if !ok {
			continue
		}
		item := byEscaped[escaped]
		if item == nil {
			identity, err := instance.Unescape(escaped)
			if err != nil {
				return nil, fmt.Errorf("unit %s: %w", u.Name, err)
			}
			item = &Instance{Identity: identity, Escaped: escaped}
			byEscaped[escaped] = item
		}
		switch kind {
		case "service":
			item.Service = u
		case "socket":
			item.Socket = u
		}
	}
	out := make([]Instance, 0, len(byEscaped))
	for _, item := range byEscaped {
		out = append(out, *item)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Identity < out[j].Identity })
	return out, nil
}

func parsePSBoxUnit(name string) (escaped, kind string, ok bool) {
	if !strings.HasPrefix(name, unitPrefix) {
		return "", "", false
	}
	rest := strings.TrimPrefix(name, unitPrefix)
	switch {
	case strings.HasSuffix(rest, serviceSuffix):
		return strings.TrimSuffix(rest, serviceSuffix), "service", true
	case strings.HasSuffix(rest, socketSuffix):
		return strings.TrimSuffix(rest, socketSuffix), "socket", true
	default:
		return "", "", false
	}
}

func resolveInstance(name string, items []Instance) (Instance, error) {
	if name == "" {
		return Instance{}, fmt.Errorf("missing instance name")
	}
	for _, item := range items {
		if item.Identity == name || item.Escaped == name {
			return item, nil
		}
	}
	if !strings.Contains(name, defaultNameSep) {
		def := instance.Identity(name, config.DefaultInstance)
		for _, item := range items {
			if item.Identity == def {
				return item, nil
			}
		}
	}
	return Instance{}, fmt.Errorf("instance %q is not running", name)
}

func unitState(u systemd.Unit) string {
	if u.Name == "" {
		return missingState
	}
	if u.SubState != "" {
		return u.SubState
	}
	if u.ActiveState != "" {
		return u.ActiveState
	}
	return missingState
}

func formatPID(u systemd.Unit) string {
	if u.Name == "" || u.MainPID == 0 {
		return missingState
	}
	return strconv.FormatUint(uint64(u.MainPID), 10)
}

func formatSince(u systemd.Unit) string {
	if u.Name == "" || u.ActiveEnterTimestamp == 0 {
		return missingState
	}
	return time.Unix(int64(u.ActiveEnterTimestamp/1_000_000), 0).Local().Format("2006-01-02 15:04")
}

func formatCount(u systemd.Unit, n uint32) string {
	if u.Name == "" {
		return missingState
	}
	return strconv.FormatUint(uint64(n), 10)
}
