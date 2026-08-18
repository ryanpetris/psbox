package agent

// Fx module for the in-sandbox agent.

import (
	"context"
	"fmt"
	"log/slog"
	"os"

	"go.uber.org/fx"
	"go.uber.org/fx/fxevent"

	"petris.dev/psbox/internal/instance"
)

// Module wires psboxa.
var Module = fx.Module("agent",
	fx.Provide(
		NewLogger,
		instance.NewAgent,
	),
)

// NewLogger returns the agent JSON logger.
func NewLogger() *slog.Logger {
	return slog.New(slog.NewJSONHandler(os.Stderr, &slog.HandlerOptions{
		Level: slog.LevelInfo,
	}))
}

// Run starts psboxa.
func Run() int {
	var svc *instance.Agent
	app := fx.New(
		Module,
		fx.Populate(&svc),
		fx.WithLogger(func(l *slog.Logger) fxevent.Logger {
			sl := &fxevent.SlogLogger{Logger: l}
			sl.UseLogLevel(slog.LevelDebug)
			return sl
		}),
	)
	if err := app.Err(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	ctx := context.Background()
	if err := app.Start(ctx); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	defer func() { _ = app.Stop(ctx) }()
	if err := svc.Run(ctx); err != nil {
		svcLog := slog.New(slog.NewJSONHandler(os.Stderr, nil))
		svcLog.Error("psboxa exited", "error", err)
		return 1
	}
	return 0
}
