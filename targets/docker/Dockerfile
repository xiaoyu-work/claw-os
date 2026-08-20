FROM scratch

ARG ROOTFS_DIR=build/claw-os-rootfs
COPY ${ROOTFS_DIR}/ /
COPY --chmod=0755 targets/docker/container-entrypoint.sh /usr/local/sbin/claw-container-entrypoint

ENV container=docker
ENV NODE_MAJOR=24
ENV CLAW_UID=1000
ENV CLAW_GID=1000
ENV COS_BROWSER_BIN=cos-browser
ENV COS_BROWSER_PORT=9222

STOPSIGNAL SIGRTMIN+3
WORKDIR /workspace
VOLUME ["/sys/fs/cgroup"]
ENTRYPOINT ["/usr/local/sbin/claw-container-entrypoint"]
CMD ["/sbin/init"]
