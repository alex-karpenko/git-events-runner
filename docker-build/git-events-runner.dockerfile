# Build stage
FROM rust:1.82 AS build

WORKDIR /app
COPY . /app

ARG RUSTFLAGS='-C target-feature=+crt-static'

RUN cargo build --target x86_64-unknown-linux-gnu --release --bin git-events-runner
RUN strip target/x86_64-unknown-linux-gnu/release/git-events-runner

# Runtime stage
FROM gcr.io/distroless/cc-debian12
COPY --from=build /app/target/x86_64-unknown-linux-gnu/release/git-events-runner /

ENTRYPOINT ["/git-events-runner"]
CMD ["--help"]
