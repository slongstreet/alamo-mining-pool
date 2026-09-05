# Litecoin Core from the official release tarballs, multi-arch.
FROM debian:bookworm-slim AS fetch
ARG LITECOIN_VERSION=0.21.5.6
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN case "$TARGETARCH" in \
      amd64) ARCH=x86_64 ;; \
      arm64) ARCH=aarch64 ;; \
      *) echo "unsupported arch $TARGETARCH" && exit 1 ;; \
    esac \
 && curl -fsSL "https://download.litecoin.org/litecoin-${LITECOIN_VERSION}/linux/litecoin-${LITECOIN_VERSION}-${ARCH}-linux-gnu.tar.gz" -o /tmp/ltc.tgz \
 && mkdir -p /opt/litecoin && tar -xzf /tmp/ltc.tgz -C /opt/litecoin --strip-components=1

FROM debian:bookworm-slim
RUN useradd --system --uid 10002 --create-home litecoin && mkdir -p /home/litecoin/.litecoin && chown litecoin:litecoin /home/litecoin/.litecoin
COPY --from=fetch /opt/litecoin/bin/litecoind /opt/litecoin/bin/litecoin-cli /usr/local/bin/
USER litecoin
VOLUME /home/litecoin/.litecoin
EXPOSE 19443 19444
ENTRYPOINT ["litecoind"]
