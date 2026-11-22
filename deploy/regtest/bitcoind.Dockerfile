# Bitcoin Core from the official release tarballs, multi-arch, checked against the
# release's SHA256SUMS (https://bitcoincore.org/bin/bitcoin-core-32.1/SHA256SUMS).
FROM debian:bookworm-slim AS fetch
ARG BITCOIN_VERSION=32.1
ARG BITCOIN_SHA256_X86_64=61545c17c3120fb7412f896d248b11993af04ec8817aad3649c55b2afb044963
ARG BITCOIN_SHA256_AARCH64=7ed37c7373eb6aa3e3f72842596a1afeba1b223717033cade9842b25773e1922
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN case "$TARGETARCH" in \
      amd64) ARCH=x86_64; SHA256="$BITCOIN_SHA256_X86_64" ;; \
      arm64) ARCH=aarch64; SHA256="$BITCOIN_SHA256_AARCH64" ;; \
      *) echo "unsupported arch $TARGETARCH" && exit 1 ;; \
    esac \
 && curl -fsSL --retry 5 --retry-all-errors --speed-limit 1024 --speed-time 60 "https://bitcoincore.org/bin/bitcoin-core-${BITCOIN_VERSION}/bitcoin-${BITCOIN_VERSION}-${ARCH}-linux-gnu.tar.gz" -o /tmp/btc.tgz \
 && echo "${SHA256}  /tmp/btc.tgz" | sha256sum -c - \
 && mkdir -p /opt/bitcoin && tar -xzf /tmp/btc.tgz -C /opt/bitcoin --strip-components=1

FROM debian:bookworm-slim
RUN useradd --system --uid 10004 --create-home bitcoin && mkdir -p /home/bitcoin/.bitcoin && chown bitcoin:bitcoin /home/bitcoin/.bitcoin
COPY --from=fetch /opt/bitcoin/bin/bitcoind /opt/bitcoin/bin/bitcoin-cli /usr/local/bin/
USER bitcoin
VOLUME /home/bitcoin/.bitcoin
EXPOSE 18443 18444
ENTRYPOINT ["bitcoind"]
