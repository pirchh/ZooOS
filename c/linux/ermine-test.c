/* ErmineTest: a plain Linux program using Unix sockets, descriptor
 * passing, poll, select, epoll and eventfd (docs/linux7-design.md), the
 * pieces Wine's server and event loops are built on. Nothing in it knows
 * about ZooOS. The exit code names the first failure. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int fail(int code, const char *what) {
    printf("ErmineTest: FAILED %s (errno %d)\n", what, errno);
    return code;
}

static unsigned long long cycles(void) { return __builtin_ia32_rdtsc(); }

static long long now_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (long long)t.tv_sec * 1000 + t.tv_nsec / 1000000;
}

/* Sends `n` bytes and (if fd >= 0) one descriptor. */
static int send_fd(int sock, const void *buf, size_t n, int fd) {
    struct iovec iov = {(void *)buf, n};
    union {
        struct cmsghdr h;
        char space[CMSG_SPACE(sizeof(int))];
    } ctl;
    struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1};
    if (fd >= 0) {
        m.msg_control = &ctl;
        m.msg_controllen = sizeof ctl.space;
        struct cmsghdr *c = CMSG_FIRSTHDR(&m);
        c->cmsg_level = SOL_SOCKET;
        c->cmsg_type = SCM_RIGHTS;
        c->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(c), &fd, sizeof fd);
    }
    return (int)sendmsg(sock, &m, 0);
}

/* Receives up to `n` bytes; *fd gets a received descriptor or -1. */
static int recv_fd(int sock, void *buf, size_t n, int *fd) {
    struct iovec iov = {buf, n};
    union {
        struct cmsghdr h;
        char space[CMSG_SPACE(sizeof(int) * 4)];
    } ctl;
    struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = &ctl,
                       .msg_controllen = sizeof ctl.space};
    int r = (int)recvmsg(sock, &m, 0);
    *fd = -1;
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    if (c && c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS) memcpy(fd, CMSG_DATA(c), sizeof(int));
    return r;
}

static int efd_shared;
static void *poke(void *arg) {
    (void)arg;
    struct timespec t = {0, 30 * 1000000};
    nanosleep(&t, NULL);
    uint64_t one = 1;
    write(efd_shared, &one, 8);
    return NULL;
}

int main(void) {
    fflush(stdout);
    int status;
    char buf[256];

    /* 1. socketpair both ways, and a pipe passed across it. */
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv)) return fail(10, "socketpair");
    if (write(sv[0], "abc", 3) != 3 || read(sv[1], buf, sizeof buf) != 3 || memcmp(buf, "abc", 3))
        return fail(11, "socketpair a->b");
    if (write(sv[1], "xyz", 3) != 3 || read(sv[0], buf, sizeof buf) != 3 || memcmp(buf, "xyz", 3))
        return fail(12, "socketpair b->a");
    int p[2];
    if (pipe(p)) return fail(13, "pipe");
    if (send_fd(sv[0], "F", 1, p[0]) != 1) return fail(14, "sendmsg SCM_RIGHTS");
    close(p[0]);
    int got;
    if (recv_fd(sv[1], buf, sizeof buf, &got) != 1 || got < 0) return fail(15, "recvmsg SCM_RIGHTS");
    write(p[1], "via fd", 6);
    if (read(got, buf, sizeof buf) != 6 || memcmp(buf, "via fd", 6)) return fail(16, "read the passed fd");
    close(got);
    close(p[1]);

    /* 2. A named socket: bind, listen, a forked client connects and sends
     * data and a descriptor; accept; reply. */
    struct sockaddr_un addr = {.sun_family = AF_UNIX};
    strcpy(addr.sun_path, "/home/user/ermine.sock");
    int ls = socket(AF_UNIX, SOCK_STREAM, 0);
    if (ls < 0 || bind(ls, (struct sockaddr *)&addr, sizeof addr)) return fail(20, "bind");
    int dup_ls = socket(AF_UNIX, SOCK_STREAM, 0);
    errno = 0;
    if (bind(dup_ls, (struct sockaddr *)&addr, sizeof addr) == 0 || errno != EADDRINUSE)
        return fail(21, "EADDRINUSE");
    close(dup_ls);
    if (listen(ls, 8)) return fail(22, "listen");
    /* The name is visible as a socket (Wine's client checks with lstat). */
    struct stat sst;
    if (lstat(addr.sun_path, &sst) || !S_ISSOCK(sst.st_mode)) return fail(19, "lstat of a bound socket");
    pid_t c = fork();
    if (c == 0) {
        int s = socket(AF_UNIX, SOCK_STREAM, 0);
        if (connect(s, (struct sockaddr *)&addr, sizeof addr)) _exit(1);
        int q[2];
        pipe(q);
        write(q[1], "from child", 10);
        close(q[1]);
        if (send_fd(s, "hello", 5, q[0]) != 5) _exit(2);
        char r[16];
        if (read(s, r, sizeof r) != 4 || memcmp(r, "pong", 4)) _exit(3);
        _exit(0);
    }
    struct pollfd pf = {ls, POLLIN, 0};
    if (poll(&pf, 1, 2000) != 1 || !(pf.revents & POLLIN)) return fail(23, "poll on listener");
    int cs = accept4(ls, NULL, NULL, SOCK_CLOEXEC);
    if (cs < 0) return fail(24, "accept");
    if (recv_fd(cs, buf, sizeof buf, &got) != 5 || memcmp(buf, "hello", 5) || got < 0) return fail(25, "child's message");
    int n = (int)read(got, buf, sizeof buf);
    if (n != 10 || memcmp(buf, "from child", 10)) return fail(26, "child's pipe");
    close(got);
    struct ucred cred;
    socklen_t cl = sizeof cred;
    if (getsockopt(cs, SOL_SOCKET, SO_PEERCRED, &cred, &cl) || cred.pid != c) return fail(27, "SO_PEERCRED");
    write(cs, "pong", 4);
    if (waitpid(c, &status, 0) != c || WEXITSTATUS(status) != 0) return fail(28, "client");
    /* The client is gone: end of data and a hang-up. */
    if (read(cs, buf, sizeof buf) != 0) return fail(29, "EOF after the peer closed");
    close(cs);
    close(ls);
    int s2 = socket(AF_UNIX, SOCK_STREAM, 0);
    errno = 0;
    if (connect(s2, (struct sockaddr *)&addr, sizeof addr) == 0 || errno != ECONNREFUSED)
        return fail(30, "ECONNREFUSED after close");
    close(s2);

    /* 3. poll: timeout, data, hang-up. */
    if (pipe(p)) return fail(40, "pipe");
    struct pollfd pp = {p[0], POLLIN, 0};
    long long t0 = now_ms();
    if (poll(&pp, 1, 60) != 0) return fail(41, "poll timeout");
    if (now_ms() - t0 < 50) return fail(42, "poll returned early");
    write(p[1], "d", 1);
    if (poll(&pp, 1, -1) != 1 || pp.revents != POLLIN) return fail(43, "poll POLLIN");
    read(p[0], buf, 1);
    close(p[1]);
    if (poll(&pp, 1, 0) != 1 || !(pp.revents & POLLHUP)) return fail(44, "poll POLLHUP");
    close(p[0]);

    /* 4. select. */
    if (pipe(p)) return fail(50, "pipe");
    fd_set rs, ws;
    FD_ZERO(&rs);
    FD_ZERO(&ws);
    FD_SET(p[0], &rs);
    FD_SET(p[1], &ws);
    if (select(p[1] + 1, &rs, &ws, NULL, NULL) != 1 || FD_ISSET(p[0], &rs) || !FD_ISSET(p[1], &ws))
        return fail(51, "select writable");
    write(p[1], "s", 1);
    FD_ZERO(&rs);
    FD_SET(p[0], &rs);
    struct timeval tv = {0, 0};
    if (select(p[0] + 1, &rs, NULL, NULL, &tv) != 1 || !FD_ISSET(p[0], &rs)) return fail(52, "select readable");
    close(p[0]);
    close(p[1]);

    /* 5. Non-blocking: EAGAIN on an empty pipe and a full eventfd read. */
    if (pipe2(p, O_NONBLOCK)) return fail(60, "pipe2 O_NONBLOCK");
    errno = 0;
    if (read(p[0], buf, 1) != -1 || errno != EAGAIN) return fail(61, "EAGAIN pipe");
    int fl = fcntl(p[0], F_GETFL);
    if (!(fl & O_NONBLOCK)) return fail(62, "F_GETFL O_NONBLOCK");
    fcntl(p[0], F_SETFL, fl & ~O_NONBLOCK);
    if (fcntl(p[0], F_GETFL) & O_NONBLOCK) return fail(63, "F_SETFL");
    close(p[0]);
    close(p[1]);

    /* 6. eventfd: counter and semaphore. */
    int e = eventfd(0, EFD_NONBLOCK);
    uint64_t v = 3;
    write(e, &v, 8);
    v = 4;
    write(e, &v, 8);
    if (read(e, &v, 8) != 8 || v != 7) return fail(70, "eventfd sum");
    errno = 0;
    if (read(e, &v, 8) != -1 || errno != EAGAIN) return fail(71, "eventfd EAGAIN");
    close(e);
    e = eventfd(2, EFD_SEMAPHORE);
    if (read(e, &v, 8) != 8 || v != 1 || read(e, &v, 8) != 8 || v != 1) return fail(72, "eventfd semaphore");
    close(e);

    /* 7. epoll: nothing ready, eventfd ready with its data, ONESHOT. */
    int ep = epoll_create1(EPOLL_CLOEXEC);
    e = eventfd(0, 0);
    if (pipe(p)) return fail(80, "pipe");
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 0x1234};
    if (epoll_ctl(ep, EPOLL_CTL_ADD, e, &ev)) return fail(81, "epoll_ctl ADD");
    ev.events = EPOLLIN | EPOLLONESHOT;
    ev.data.u64 = 0x5678;
    if (epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &ev)) return fail(82, "epoll_ctl ADD pipe");
    errno = 0;
    if (epoll_ctl(ep, EPOLL_CTL_ADD, e, &ev) == 0 || errno != EEXIST) return fail(83, "EEXIST");
    struct epoll_event out[4];
    if (epoll_wait(ep, out, 4, 0) != 0) return fail(84, "epoll nothing ready");
    v = 1;
    write(e, &v, 8);
    write(p[1], "e", 1);
    n = epoll_wait(ep, out, 4, 1000);
    if (n != 2) return fail(85, "epoll two ready");
    int seen = 0;
    for (int k = 0; k < n; k++) seen |= (out[k].data.u64 == 0x1234) | (out[k].data.u64 == 0x5678) << 1;
    if (seen != 3) return fail(86, "epoll data");
    read(e, &v, 8);
    /* The pipe still has data, but ONESHOT disabled it. */
    if (epoll_wait(ep, out, 4, 0) != 0) return fail(87, "EPOLLONESHOT");
    ev.events = EPOLLIN;
    epoll_ctl(ep, EPOLL_CTL_MOD, p[0], &ev);
    if (epoll_wait(ep, out, 4, 0) != 1) return fail(88, "EPOLL_CTL_MOD re-arms");

    /* 8. A thread wakes another's epoll_wait through an eventfd. */
    efd_shared = e;
    pthread_t t;
    pthread_create(&t, NULL, poke, NULL);
    read(p[0], buf, 1);
    epoll_ctl(ep, EPOLL_CTL_DEL, p[0], NULL);
    n = epoll_wait(ep, out, 4, 2000);
    pthread_join(t, NULL);
    if (n != 1 || out[0].data.u64 != 0x1234) return fail(89, "epoll woken by a thread");
    close(ep);
    close(e);
    close(p[0]);
    close(p[1]);

    /* 9. Timing: 1000 round trips between two processes on a socketpair. */
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) return fail(90, "socketpair");
    c = fork();
    if (c == 0) {
        close(sv[0]);
        char b;
        while (read(sv[1], &b, 1) == 1) write(sv[1], &b, 1);
        _exit(0);
    }
    close(sv[1]);
    unsigned long long c0 = cycles();
    for (int k = 0; k < 1000; k++) {
        char b = (char)k;
        if (write(sv[0], &b, 1) != 1 || read(sv[0], &b, 1) != 1 || b != (char)k) return fail(91, "ping-pong");
    }
    unsigned long long c1 = cycles();
    close(sv[0]);
    if (waitpid(c, &status, 0) != c || WEXITSTATUS(status) != 0) return fail(92, "echo child");
    printf("ErmineTest: socket round trip %llu cycles (average of 1000)\n", (c1 - c0) / 1000);
    printf("ErmineTest: all checks passed\n");
    return 0;
}
