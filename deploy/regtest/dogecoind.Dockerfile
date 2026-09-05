# Dogecoin Core from the official release tarballs, multi-arch.
FROM debian:bookworm-slim AS fetch
ARG DOGECOIN_VERSION=1.14.9
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN case "$TARGETARCH" in \
      amd64) ARCH=x86_64 ;; \
      arm64) ARCH=aarch64 ;; \
      *) echo "unsupported arch $TARGETARCH" && exit 1 ;; \
    esac \
 && curl -fsSL "https://github.com/dogecoin/dogecoin/releases/download/v${DOGECOIN_VERSION}/dogecoin-${DOGECOIN_VERSION}-${ARCH}-linux-gnu.tar.gz" -o /tmp/doge.tgz \
 && mkdir -p /opt/dogecoin && tar -xzf /tmp/doge.tgz -C /opt/dogecoin --strip-components=1

FROM debian:bookworm-slim
RUN useradd --system --uid 10003 --create-home dogecoin && mkdir -p /home/dogecoin/.dogecoin && chown dogecoin:dogecoin /home/dogecoin/.dogecoin
COPY --from=fetch /opt/dogecoin/bin/dogecoind /opt/dogecoin/bin/dogecoin-cli /usr/local/bin/
USER dogecoin
VOLUME /home/dogecoin/.dogecoin
EXPOSE 18332 18444
ENTRYPOINT ["dogecoind"]
