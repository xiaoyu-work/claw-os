FROM scratch

COPY build/claw-os-rootfs /

ENV NODE_MAJOR=24
ENV DEN=/den
ENV COS_BROWSER_BIN=cos-browser
ENV COS_BROWSER_PORT=9222

WORKDIR /den
ENTRYPOINT ["/usr/local/bin/cos-init"]
CMD ["/bin/bash", "--login"]
