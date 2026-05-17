FROM scratch

COPY build/claw-os-rootfs /

ENV container=docker
ENV NODE_MAJOR=24
ENV HOME=/home/cos
ENV COS_HOME=/home/cos
ENV COS_BROWSER_BIN=cos-browser
ENV COS_BROWSER_PORT=9222

STOPSIGNAL SIGRTMIN+3
WORKDIR /
VOLUME ["/sys/fs/cgroup"]
CMD ["/sbin/init"]
