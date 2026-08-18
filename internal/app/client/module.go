package client

// Fx module for the psbox CLI.

import (
	"context"
	"fmt"
	"log/slog"
	"os"

	"github.com/spf13/cobra"
	"go.uber.org/fx"
	"go.uber.org/fx/fxevent"

	"petris.dev/psbox/internal/cli"
	"petris.dev/psbox/internal/config"
	"petris.dev/psbox/internal/instance"
	"petris.dev/psbox/internal/launch"
	"petris.dev/psbox/internal/objects"
	"petris.dev/psbox/internal/sandbox"
	"petris.dev/psbox/internal/systemd"
	"petris.dev/psbox/internal/units"
)

// Module wires the psbox CLI.
var Module = fx.Module("client",
	fx.Provide(
		NewLogger,
		config.LoadPaths,
		config.NewLoader,
		systemd.NewUser,
		provideStarter,
		provideControl,
		instance.NewClient,
		units.NewService,
		launch.NewService,
		objects.NewService,
		sandbox.NewService,
		cli.NewRoot,
	),
)

func provideStarter(u *systemd.User) instance.Starter { return u }

func provideControl(u *systemd.User) systemd.Control { return u }

// NewLogger returns the CLI structured logger.
func NewLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{
		Level: slog.LevelInfo,
	}))
}

// Run constructs the CLI and executes it. It returns a process exit code.
func Run() int {
	var root *cobra.Command
	app := fx.New(
		Module,
		fx.Populate(&root),
		fx.WithLogger(func(log *slog.Logger) fxevent.Logger {
			l := &fxevent.SlogLogger{Logger: log}
			l.UseLogLevel(slog.LevelDebug)
			return l
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
	return cli.Execute(root)
}
