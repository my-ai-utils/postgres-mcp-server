FROM ubuntu:22.04
COPY ./target/release/postgres-mcp-server ./target/release/postgres-mcp-server
ENTRYPOINT ["./target/release/postgres-mcp-server"]