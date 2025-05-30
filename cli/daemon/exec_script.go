package daemon

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/rs/zerolog/log"
	"golang.org/x/mod/modfile"

	"encr.dev/cli/daemon/run"
	"encr.dev/internal/optracker"
	"encr.dev/pkg/appfile"
	"encr.dev/pkg/paths"
	daemonpb "encr.dev/proto/encore/daemon"
)

// ExecScript executes a one-off script.
func (s *Server) ExecScript(req *daemonpb.ExecScriptRequest, stream daemonpb.Daemon_ExecScriptServer) error {
	ctx := stream.Context()
	slog := &streamLog{stream: stream, buffered: true}
	stderr := slog.Stderr(false)
	sendErr := func(err error) {
		if list := run.AsErrorList(err); list != nil {
			_ = list.SendToStream(stream)
		} else {
			errStr := err.Error()
			if !strings.HasSuffix(errStr, "\n") {
				errStr += "\n"
			}
			slog.Stderr(false).Write([]byte(errStr))
		}
		streamExit(stream, 1)
	}

	ctx, tracer, err := s.beginTracing(ctx, req.AppRoot, req.WorkingDir, req.TraceFile)
	if err != nil {
		sendErr(err)
		return nil
	}
	defer tracer.Close()

	app, err := s.apps.Track(req.AppRoot)
	if err != nil {
		sendErr(err)
		return nil
	}

	ns, err := s.namespaceOrActive(ctx, app, req.Namespace)
	if err != nil {
		sendErr(err)
		return nil
	}

	ops := optracker.New(stderr, stream)
	defer ops.AllDone() // Kill the tracker when we exit this function

	testResults := make(chan error, 1)
	defer func() {
		if recovered := recover(); recovered != nil {
			var err error
			switch recovered := recovered.(type) {
			case error:
				err = recovered
			default:
				err = fmt.Errorf("%v", recovered)
			}
			log.Err(err).Msg("panic during script execution")
			testResults <- fmt.Errorf("panic occured within Encore during script execution: %v\n", recovered)
		}
	}()

	switch app.Lang() {
	case appfile.LangGo:
		modPath := filepath.Join(app.Root(), "go.mod")
		modData, err := os.ReadFile(modPath)
		if err != nil {
			sendErr(err)
			return nil
		}
		mod, err := modfile.Parse(modPath, modData, nil)
		if err != nil {
			sendErr(err)
			return nil
		}

		commandRelPath := filepath.ToSlash(filepath.Join(req.WorkingDir, req.ScriptArgs[0]))
		scriptArgs := req.ScriptArgs[1:]
		commandPkg := paths.Pkg(mod.Module.Mod.Path).JoinSlash(paths.RelSlash(commandRelPath))

		p := run.ExecScriptParams{
			App:        app,
			NS:         ns,
			WorkingDir: req.WorkingDir,
			Environ:    req.Environ,
			MainPkg:    commandPkg,
			ScriptArgs: scriptArgs,
			Stdout:     slog.Stdout(false),
			Stderr:     slog.Stderr(false),
			OpTracker:  ops,
		}
		if err := s.mgr.ExecScript(stream.Context(), p); err != nil {
			sendErr(err)
		} else {
			streamExit(stream, 0)
		}
	case appfile.LangTS:
		p := run.ExecCommandParams{
			App:        app,
			NS:         ns,
			WorkingDir: req.WorkingDir,
			Environ:    req.Environ,
			Command:    req.ScriptArgs[0],
			ScriptArgs: req.ScriptArgs[1:],
			Stdout:     slog.Stdout(false),
			Stderr:     slog.Stderr(false),
			OpTracker:  ops,
		}

		if err := s.mgr.ExecCommand(stream.Context(), p); err != nil {
			sendErr(err)
		} else {
			streamExit(stream, 0)
		}
	}

	return nil
}
