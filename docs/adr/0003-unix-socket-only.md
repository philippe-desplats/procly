# Unix socket only, no TCP control port

The Daemon listens only on a `0600` Unix socket under the user's state directory, never on a TCP port. Portly's loopback HTTP API accepts a cross-origin `text/plain` POST and runs the command it carries, which was reproduced against its Go daemon, so any browser tab can execute commands. An authenticated HTTP API is added only when a client genuinely needs it, and never without a token.
