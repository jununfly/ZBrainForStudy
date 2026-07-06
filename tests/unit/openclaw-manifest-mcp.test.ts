/**
 * Guards the real openclaw.plugin.json mcpServers block against two live
 * breaks discovered during the release-infra migration:
 *
 *   1. Semantic break: the manifest launched the MCP server with
 *      args ["serve"]. On the Rust binary `serve` is the HTTP API server,
 *      while the stdio MCP server (what an MCP client expects) is
 *      `serve-mcp`. So an MCP client wiring up this manifest would get an
 *      HTTP server on stdin/stdout instead of a JSON-RPC stdio server.
 *
 *   2. Path break: the manifest's command was "./bin/zbrain" — a file that
 *      does not exist. npm links `zbrain` onto PATH via package.json bin,
 *      but openclaw execs the manifest command as a literal relative path
 *      and never resolves that symlink. The only real entry is the Node
 *      wrapper bin/zbrain-rs.js (platform detection + cargo fallback).
 *
 * The fix routes the manifest through the existing wrapper via an explicit
 * `node` command (not a shebang — Windows has no .cmd sibling for a
 * bare bin/zbrain-rs.js). This test locks both invariants and asserts the
 * referenced wrapper file actually exists on disk.
 */

import { describe, expect, it } from 'bun:test';
import { existsSync, readFileSync } from 'fs';
import { join, isAbsolute } from 'path';
import { fileURLToPath } from 'url';

const repoRoot = join(fileURLToPath(new URL('.', import.meta.url)), '..', '..');

interface McpServerEntry {
  command: string;
  args?: string[];
}

function loadManifestMcpServer(): McpServerEntry {
  const manifestPath = join(repoRoot, 'openclaw.plugin.json');
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8')) as {
    mcpServers?: Record<string, McpServerEntry>;
  };
  const entry = manifest.mcpServers?.zbrain;
  if (!entry) throw new Error('openclaw.plugin.json missing mcpServers.zbrain');
  return entry;
}

describe('openclaw.plugin.json mcpServers.zbrain', () => {
  it('launches the stdio MCP server via serve-mcp, not the HTTP serve command', () => {
    const entry = loadManifestMcpServer();
    const args = entry.args ?? [];
    expect(args).toContain('serve-mcp');
    expect(args).not.toContain('serve');
  });

  it('invokes the wrapper through node (portable across Windows/unix)', () => {
    const entry = loadManifestMcpServer();
    expect(entry.command).toBe('node');
  });

  it('references the zbrain-rs.js wrapper, which exists on disk', () => {
    const entry = loadManifestMcpServer();
    const args = entry.args ?? [];
    const wrapperArg = args.find((a) => a.includes('zbrain-rs.js'));
    expect(wrapperArg).toBeDefined();

    const wrapperPath = isAbsolute(wrapperArg!)
      ? wrapperArg!
      : join(repoRoot, wrapperArg!);
    expect(existsSync(wrapperPath)).toBe(true);
  });
});
