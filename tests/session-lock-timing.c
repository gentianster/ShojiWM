/* Regression probe: a timed lock-surface commit must advance without input.
 * Connects only to an absolute socket whose peer PID was supplied by the
 * isolated test supervisor. No credentials or PAM calls are involved. */
#define _GNU_SOURCE
#include <wayland-client.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/mman.h>
#include <poll.h>
#include <time.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include "session-lock.h"
#include "commit-timing.h"

static struct wl_display *display;
static struct wl_compositor *compositor;
static struct wl_shm *shm;
static struct wl_output *output;
static struct ext_session_lock_manager_v1 *lock_manager;
static struct wp_commit_timing_manager_v1 *timing_manager;
static struct ext_session_lock_v1 *lock;
static struct wl_surface *surface;
static struct wp_commit_timer_v1 *timer;
static struct wl_buffer *buffers[2];
static int phase, failed, completed;
static long delay_ms;
static int64_t start_ns, submit_ns, target_ns;
static int64_t now_ns(void) {
    struct timespec t; clock_gettime(CLOCK_MONOTONIC,&t);
    return (int64_t)t.tv_sec*1000000000+t.tv_nsec;
}
static void global(void *data,struct wl_registry *r,uint32_t name,const char *i,uint32_t v) {
    if(!strcmp(i,"wl_compositor")) compositor=wl_registry_bind(r,name,&wl_compositor_interface,4);
    if(!strcmp(i,"wl_shm")) shm=wl_registry_bind(r,name,&wl_shm_interface,1);
    if(!strcmp(i,"wl_output") && !output) output=wl_registry_bind(r,name,&wl_output_interface,1);
    if(!strcmp(i,"ext_session_lock_manager_v1")) lock_manager=wl_registry_bind(r,name,&ext_session_lock_manager_v1_interface,1);
    if(!strcmp(i,"wp_commit_timing_manager_v1")) timing_manager=wl_registry_bind(r,name,&wp_commit_timing_manager_v1_interface,1);
}
static void removed(void *d,struct wl_registry *r,uint32_t n) {}
static const struct wl_registry_listener registry_listener={global,removed};
static void frame_done(void *data,struct wl_callback *cb,uint32_t time);
static const struct wl_callback_listener frame_listener={frame_done};
static void submit(int which) {
    struct wl_callback *cb=wl_surface_frame(surface);
    wl_callback_add_listener(cb,&frame_listener,NULL);
    wl_surface_attach(surface,buffers[which],0,0);
    wl_surface_damage(surface,0,0,INT32_MAX,INT32_MAX);
    wl_surface_commit(surface);
    wl_display_flush(display);
}
static void frame_done(void *data,struct wl_callback *cb,uint32_t time) {
    wl_callback_destroy(cb);
    if(phase==0) {
        phase=1;
        submit_ns=now_ns(); target_ns=submit_ns+delay_ms*1000000;
        /* Negative delay covers an already-expired deadline. */
        uint64_t sec=target_ns/1000000000;
        wp_commit_timer_v1_set_timestamp(timer,sec>>32,sec&UINT32_MAX,target_ns%1000000000);
        printf("FIRST_FRAME; TIMED_COMMIT delay_ms=%ld\n",delay_ms);fflush(stdout);
        submit(1);
    } else {
        int64_t elapsed=now_ns()-submit_ns;
        /* pre_repaint may target the next vblank; allow 25ms of lead. */
        failed=delay_ms>0 && elapsed+25000000<delay_ms*1000000;
        printf("SECOND_FRAME elapsed_ms=%.3f early=%d\n",elapsed/1e6,failed);fflush(stdout);
        ext_session_lock_v1_unlock_and_destroy(lock);
        wl_display_flush(display);
        completed=1;
    }
}
static void configured(void *d,struct ext_session_lock_surface_v1 *ls,uint32_t serial,uint32_t w,uint32_t h) {
    ext_session_lock_surface_v1_ack_configure(ls,serial);
    size_t bytes=(size_t)w*h*4;
    int fd=memfd_create("isolated-timing-probe",MFD_CLOEXEC);
    if(fd<0 || ftruncate(fd,2*bytes)) {failed=1;completed=1;return;}
    uint32_t *pixels=mmap(NULL,2*bytes,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);
    if(pixels==MAP_FAILED) {failed=1;completed=1;close(fd);return;}
    for(size_t i=0;i<bytes/4;i++) {pixels[i]=0xff702030;pixels[bytes/4+i]=0xff207050;}
    struct wl_shm_pool *pool=wl_shm_create_pool(shm,fd,2*bytes);
    buffers[0]=wl_shm_pool_create_buffer(pool,0,w,h,w*4,WL_SHM_FORMAT_ARGB8888);
    buffers[1]=wl_shm_pool_create_buffer(pool,bytes,w,h,w*4,WL_SHM_FORMAT_ARGB8888);
    wl_shm_pool_destroy(pool);munmap(pixels,2*bytes);close(fd);
    submit(0);
}
static const struct ext_session_lock_surface_v1_listener surface_listener={configured};
static void locked(void *d,struct ext_session_lock_v1 *l) {
    surface=wl_compositor_create_surface(compositor);
    timer=wp_commit_timing_manager_v1_get_timer(timing_manager,surface);
    struct ext_session_lock_surface_v1 *ls=ext_session_lock_v1_get_lock_surface(l,surface,output);
    ext_session_lock_surface_v1_add_listener(ls,&surface_listener,NULL);
}
static void finished(void *d,struct ext_session_lock_v1 *l) {failed=1;completed=1;}
static const struct ext_session_lock_v1_listener lock_listener={locked,finished};
int main(int argc,char **argv) {
    if(argc!=4 || argv[1][0]!='/') return 2;
    delay_ms=strtol(argv[3],NULL,10);
    if(delay_ms < -1000 || delay_ms > 2000) return 2;
    struct sockaddr_un addr={.sun_family=AF_UNIX};
    if(strlen(argv[1])>=sizeof(addr.sun_path)) return 3;
    strcpy(addr.sun_path,argv[1]);
    int fd=socket(AF_UNIX,SOCK_STREAM|SOCK_CLOEXEC,0);
    if(connect(fd,(struct sockaddr *)&addr,sizeof(addr))) {perror("connect");return 4;}
    struct ucred cred; socklen_t len=sizeof(cred);
    if(getsockopt(fd,SOL_SOCKET,SO_PEERCRED,&cred,&len) || cred.pid!=atoi(argv[2]) || cred.uid!=getuid()) return 5;
    display=wl_display_connect_to_fd(fd);
    struct wl_registry *registry=wl_display_get_registry(display);
    wl_registry_add_listener(registry,&registry_listener,NULL);
    if(wl_display_roundtrip(display)<0 || !compositor || !shm || !output || !lock_manager || !timing_manager) return 6;
    lock=ext_session_lock_manager_v1_lock(lock_manager);
    ext_session_lock_v1_add_listener(lock,&lock_listener,NULL);
    start_ns=now_ns();
    while(!completed && now_ns()-start_ns<4000000000) {
        while(wl_display_prepare_read(display)!=0) {
            if(wl_display_dispatch_pending(display)<0) goto error;
        }
        wl_display_flush(display);
        struct pollfd pfd={.fd=wl_display_get_fd(display),.events=POLLIN};
        if(poll(&pfd,1,100)>0) {
            if(wl_display_read_events(display)<0) goto error;
            if(wl_display_dispatch_pending(display)<0) goto error;
        } else wl_display_cancel_read(display);
    }
    if(!completed) {puts("FAIL timed commit did not advance in 4s");failed=1;}
    if(completed && wl_display_roundtrip(display)<0) failed=1;
    wl_display_disconnect(display);
    return failed?1:0;
error:
    fprintf(stderr,"Wayland error\n");return 7;
}
