# Образ exch: многоэтапная сборка (SPEC §12, шаг 7).
# Сборка без базы: данные запросов sqlx — в .sqlx (SQLX_OFFLINE).
#   docker build -t exch:local .
FROM rust:1.97-bookworm AS build
WORKDIR /src
# Тулчейн образа = rust-toolchain.toml; переменная не даёт rustup докачивать компоненты.
ENV SQLX_OFFLINE=true CARGO_TERM_COLOR=never RUSTUP_TOOLCHAIN=1.97.0
COPY . .
RUN cargo build --release --locked --bin exch \
 && strip target/release/exch

# distroless: без шелла и пакетного менеджера, пользователь nonroot (65532).
FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app
COPY --from=build /src/target/release/exch /usr/local/bin/exch
# Значки премиум-эмодзи для `exch emoji install` (путь по умолчанию — assets/emoji).
COPY --from=build /src/assets/emoji /app/assets/emoji
ENV RUST_LOG=info
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/exch"]
CMD ["run"]
