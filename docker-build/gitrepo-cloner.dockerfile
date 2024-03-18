# Build stage
FROM rust:1.76 as build

WORKDIR /app
COPY . /app
RUN cargo build --release --bin gitrepo-cloner

# Runtime stage
FROM gcr.io/distroless/cc-debian12
COPY --from=build /app/target/release/gitrepo-cloner /

ENTRYPOINT ["/gitrepo-cloner"]
CMD ["--help"]
