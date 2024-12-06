# Build stage
FROM rust:1.83 AS build

WORKDIR /app
COPY . /app

ARG RUSTFLAGS='-C target-feature=+crt-static'

RUN cargo build --target x86_64-unknown-linux-gnu --release --bin gitrepo-cloner
RUN strip target/x86_64-unknown-linux-gnu/release/gitrepo-cloner

# Runtime stage
FROM gcr.io/distroless/cc-debian12
COPY --from=build /app/target/x86_64-unknown-linux-gnu/release/gitrepo-cloner /

ENTRYPOINT ["/gitrepo-cloner"]
CMD ["--help"]
