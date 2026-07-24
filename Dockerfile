FROM ubuntu:22.04
COPY ./target/release/postgres-mcp-server ./target/release/postgres-mcp-server
COPY ./wwwroot ./wwwroot
ENTRYPOINT ["./target/release/postgres-mcp-server"]