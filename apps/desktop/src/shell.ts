/** A word the shell reads as it is written, which needs no quotes. */
const PLAIN = /^[A-Za-z0-9_./@%+=:,-]+$/;

/** `value` as one word of a shell command line. */
export function shellWord(value: string): string {
  return PLAIN.test(value) ? value : `'${value.replaceAll("'", "'\\''")}'`;
}

/** The command that runs `procly` with `args` in the Worktree at `path`, which the developer pastes into a terminal. */
export function inWorktree(path: string, args: string[]): string {
  return `(cd ${shellWord(path)} && procly ${args.map(shellWord).join(" ")})`;
}
