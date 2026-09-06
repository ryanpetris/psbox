package instance

// SEQPACKET JSON protocol messages.

import (
	"strconv"
	"time"
)

const (
	ProtocolVersion = 1
	MaxMessageBytes = 1 << 20

	TypeSpawn         = "spawn"
	TypeSignal        = "signal"
	TypeSpawned       = "spawned"
	TypeSpawnError    = "spawn_error"
	TypeExited        = "exited"
	TypeQueryLiveness = "query_liveness"
	TypeLiveness      = "liveness"
	TypeOpenURI       = "open_uri"
	TypeOpenURIResult = "open_uri_result"

	FirstReplyTimeout = 5 * time.Second
	SpawnReadTimeout  = 5 * time.Second
	AcceptIdleTimeout = 1 * time.Second
	DefaultIdleAfter  = 60 * time.Second
)

// Message is one protocol frame.
type Message struct {
	Type        string            `json:"type"`
	Protocol    int               `json:"protocol,omitempty"`
	ID          string            `json:"id,omitempty"`
	Sandbox     string            `json:"sandbox,omitempty"`
	Instance    string            `json:"instance,omitempty"`
	Argv        []string          `json:"argv,omitempty"`
	Env         map[string]string `json:"env,omitempty"`
	Cwd         string            `json:"cwd,omitempty"`
	Signum      int               `json:"signum,omitempty"`
	PID         int               `json:"pid,omitempty"`
	ConfigHash  string            `json:"config_hash,omitempty"`
	RunningHash string            `json:"running_hash,omitempty"`
	Errno       int               `json:"errno,omitempty"`
	Message     string            `json:"message,omitempty"`
	Code        *int              `json:"code,omitempty"`
	Signal      *int              `json:"signal,omitempty"`
	Workload    bool              `json:"workload,omitempty"`
	URI         string            `json:"uri,omitempty"`
	OK          bool              `json:"ok,omitempty"`
}

// StatusError is a process exit status to propagate to the CLI.
type StatusError struct {
	Code int
}

func (e *StatusError) Error() string {
	return "exit status " + strconv.Itoa(e.Code)
}
