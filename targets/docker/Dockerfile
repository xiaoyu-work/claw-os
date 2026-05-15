FROM scratch

COPY build/claw-os-rootfs /

ENV NODE_MAJOR=24
ENV HOME=/home/cos
ENV COS_HOME=/home/cos
ENV COS_BROWSER_BIN=cos-browser
ENV COS_BROWSER_PORT=9222

WORKDIR /home/cos
USER cos
ENTRYPOINT ["/usr/local/bin/cos-init"]
CMD ["/bin/bash", "--login"]
