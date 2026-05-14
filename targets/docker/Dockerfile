FROM scratch

COPY build/claw-os-rootfs /

ENV NODE_MAJOR=24
ENV HOME=/root
ENV COS_HOME=/root
ENV COS_BROWSER_BIN=cos-browser
ENV COS_BROWSER_PORT=9222

WORKDIR /root
ENTRYPOINT ["/usr/local/bin/cos-init"]
CMD ["/bin/bash", "--login"]
