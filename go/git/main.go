// A Subako function package that clones, commits and pushes git repositories
// on the session volume with go-git, over HTTPS through the Subako worker.
package main

import (
	"net/http"

	"github.com/modelcontextprotocol/go-sdk/mcp"
	subakofn "github.com/subako-ai/subako-function-sdk-go"
)

// spoolDir holds request and response bodies while they travel through the
// worker.
const spoolDir = "/volume/tmp/.git-http"

func main() {
	t := tools{http: &http.Client{Transport: volumeTransport{dir: spoolDir, send: subakofn.Do}}}
	server := mcp.NewServer(&mcp.Implementation{Name: "git", Version: "0.1.0"}, nil)
	mcp.AddTool(server, &mcp.Tool{Name: "git_clone", Description: "Clone a git repository over HTTPS into a directory on the volume, where other tools can read and edit its files."}, t.clone)
	mcp.AddTool(server, &mcp.Tool{Name: "git_status", Description: "Show a clone's branch and commit, its local branches, and the files that differ from the last commit."}, t.status)
	mcp.AddTool(server, &mcp.Tool{Name: "git_diff", Description: "Show a unified diff of a clone's files on the volume against the last commit."}, t.diff)
	mcp.AddTool(server, &mcp.Tool{Name: "git_commit", Description: "Commit a clone's changed files, every one by default, new and deleted ones included."}, t.commit)
	mcp.AddTool(server, &mcp.Tool{Name: "git_push", Description: "Push the current branch to the same branch on the remote, creating it there if it is new."}, t.push)
	mcp.AddTool(server, &mcp.Tool{Name: "git_pull", Description: "Fast-forward the current branch to the remote's. The clone must have no uncommitted changes."}, t.pull)
	mcp.AddTool(server, &mcp.Tool{Name: "git_log", Description: "List a clone's commits, newest first, optionally only those that change one file."}, t.log)
	mcp.AddTool(server, &mcp.Tool{Name: "git_checkout", Description: "Switch a clone to another branch, or create a branch at the current commit, keeping uncommitted changes."}, t.checkout)
	subakofn.Serve(server)
}
