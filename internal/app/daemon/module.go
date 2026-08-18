package daemon

// Fx module for the host instance daemon.

import (
	"context"
	"fmt"
	"log/slog"
	"os"

	"go.uber.org/fx"
	"go.uber.org/fx/fxevent"

	"petris.dev/psbox/internal/config"
	"petris.dev/psbox/internal/instance"
)

// Module wires psboxd.
var Module = fx.Module("daemon",
	fx.Provide(
		NewLogger,
		config.LoadPaths,
		config.NewLoader,
		instance.NewDaemon,
	),
)

// NewLogger returns the daemon JSON logger.
func NewLogger() *slog.Logger {
	return slog.New(slog.NewJSONHandler(os.Stderr, &slog.HandlerOptions{
		Level: slog.LevelInfo,
	}))
}

// Run starts psboxd under socket activation.
func Run() int {
	var svc *instance.Daemon
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
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "psboxd requires the escaped instance identity")
		return 1
	}
	ctx := context.Background()
	if err := app.Start(ctx); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	defer func() { _ = app.Stop(ctx) }()
	if err := svc.Run(ctx, os.Args[1]); err != nil {
		slog.New(slog.NewJSONHandler(os.Stderr, nil)).Error("psboxd exited", "error", err)
		return 1
	}
	return 0
}
