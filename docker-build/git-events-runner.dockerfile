# Build stage
FROM rust:1.77 as build

WORKDIR /app
COPY . /app
RUN cargo build --release --bin git-events-runner

# Runtime stage
FROM gcr.io/distroless/cc-debian12
COPY --from=build /app/target/release/git-events-runner /

ENTRYPOINT ["/git-events-runner"]
CMD ["--help"]
