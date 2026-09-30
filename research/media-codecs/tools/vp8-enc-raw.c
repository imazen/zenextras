/* vp8-enc-raw.c — raw libvpx VP8 encoder oracle for zenvp8 bit-exactness work.
 *
 * Reads planar I420 raw video, encodes with a pinned deterministic
 * configuration, and writes each packet as raw bytes to stdout (IVF
 * optionally to a file). The pinned config kills every source of
 * nondeterminism/lookahead: single thread, one pass, fixed quantizer
 * bounds, lag_in_frames=0 (no altref/arnr/lookahead), no resize,
 * no dropframes, one token partition, static threshold off.
 *
 * Usage: vp8-enc-raw <in.yuv> <w> <h> <qindex> <nframes> <kf_interval> <out.ivf|->
 *
 * Packet stream on stdout is the concatenation of:
 *   u32le packet_len | u8 packet[] | u32le flags (bit0: keyframe)
 * so packets can be diffed individually.
 *
 * Build (after libvpx is configured+built, e.g. in $BUILD):
 *   cc -O2 -o vp8-enc-raw tools/vp8-enc-raw.c \
 *      -I $BUILD -I <libvpx-src> $BUILD/libvpx.a -lpthread -lm
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "vpx/vpx_encoder.h"
#include "vpx/vp8cx.h"

static void die(const char *m) {
    fprintf(stderr, "%s\n", m);
    exit(1);
}

static void ivf_header(FILE *f, int w, int h, int n) {
    unsigned char hdr[32] = {0};
    memcpy(hdr, "DKIF", 4);
    hdr[6] = 32;                    /* header size */
    memcpy(hdr + 8, "VP80", 4);     /* fourcc */
    hdr[12] = w & 0xff; hdr[13] = w >> 8;
    hdr[14] = h & 0xff; hdr[15] = h >> 8;
    hdr[16] = 30; hdr[20] = 1;      /* timebase den/num */
    hdr[24] = n & 0xff; hdr[25] = (n >> 8) & 0xff;
    hdr[26] = (n >> 16) & 0xff; hdr[27] = (n >> 24) & 0xff;
    fwrite(hdr, 1, 32, f);
}

static void ivf_frame(FILE *f, const vpx_codec_cx_pkt_t *pkt) {
    unsigned char h[12] = {0};
    size_t sz = pkt->data.frame.sz;
    h[0] = sz & 0xff; h[1] = (sz >> 8) & 0xff;
    h[2] = (sz >> 16) & 0xff; h[3] = (sz >> 24) & 0xff;
    fwrite(h, 1, 12, f);
    fwrite(pkt->data.frame.buf, 1, sz, f);
}

int main(int argc, char **argv) {
    if (argc < 7) die("usage: vp8-enc-raw <in.yuv> <w> <h> <q> <nframes> <kf_interval> [out.ivf]");
    const char *in_path = argv[1];
    const int w = atoi(argv[2]), h = atoi(argv[3]), q = atoi(argv[4]);
    const int n = atoi(argv[5]), kf_int = atoi(argv[6]);
    const char *ivf_path = argc > 7 ? argv[7] : NULL;

    FILE *in = fopen(in_path, "rb");
    if (!in) die("cannot open input");
    const int fsz = w * h * 3 / 2;
    unsigned char *buf = malloc(fsz);
    if (!buf) die("oom");

    vpx_codec_ctx_t enc;
    vpx_codec_enc_cfg_t cfg;
    vpx_codec_err_t res;
    res = vpx_codec_enc_config_default(&vpx_codec_vp8_cx_algo, &cfg, 0);
    if (res) die("config_default failed");

    cfg.g_w = w;
    cfg.g_h = h;
    cfg.g_timebase.num = 1;
    cfg.g_timebase.den = 30;
    cfg.g_lag_in_frames = 0;            /* no altref / lookahead / arnr */
    cfg.g_threads = 1;
    cfg.g_error_resilient = 0;          /* full header path — we port it all */
    cfg.g_pass = VPX_RC_ONE_PASS;
    cfg.rc_dropframe_thresh = 0;
    cfg.rc_resize_allowed = 0;
    cfg.rc_end_usage = VPX_Q;           /* constant quantizer */
    cfg.rc_min_quantizer = q;
    cfg.rc_max_quantizer = q;
    cfg.rc_target_bitrate = 1000;
    cfg.kf_mode = VPX_KF_DISABLED;      /* we force kfs explicitly */
    cfg.kf_min_dist = 9999;
    cfg.kf_max_dist = 9999;

    res = vpx_codec_enc_init(&enc, &vpx_codec_vp8_cx_algo, &cfg, 0);
    if (res) die(vpx_codec_err_to_string(res));
    vpx_codec_control(&enc, VP8E_SET_CQ_LEVEL, q);
    vpx_codec_control(&enc, VP8E_SET_CPUUSED, -5);         /* rt Speed=12: RD=0, fast quant, HEX, half-pel */
    vpx_codec_control(&enc, VP8E_SET_STATIC_THRESHOLD, 0);
    vpx_codec_control(&enc, VP8E_SET_NOISE_SENSITIVITY, 0);
    vpx_codec_control(&enc, VP8E_SET_SHARPNESS, 0);
    vpx_codec_control(&enc, VP8E_SET_TOKEN_PARTITIONS, VP8_ONE_TOKENPARTITION);
    vpx_codec_control(&enc, VP8E_SET_ARNR_MAXFRAMES, 0);
    vpx_codec_control(&enc, VP8E_SET_ARNR_STRENGTH, 0);
    vpx_codec_control(&enc, VP8E_SET_ENABLEAUTOALTREF, 0);
    vpx_codec_control(&enc, VP8E_SET_TUNING, VP8_TUNE_PSNR);

    FILE *ivf = NULL;
    if (ivf_path && strcmp(ivf_path, "-") != 0) {
        ivf = fopen(ivf_path, "wb");
        if (!ivf) die("cannot open ivf out");
        ivf_header(ivf, w, h, n);
    }

    vpx_image_t img;
    vpx_img_alloc(&img, VPX_IMG_FMT_I420, w, h, 1);

    for (int f = 0; f < n; f++) {
        if (fread(buf, 1, fsz, in) != (size_t)fsz) die("short input");
        /* copy into vpx image (planar, strides w, w/2) */
        for (int r = 0; r < h; r++)
            memcpy(img.planes[0] + r * img.stride[0], buf + r * w, w);
        const int uw = (w + 1) / 2, uh = (h + 1) / 2;
        unsigned char *up = buf + w * h;
        unsigned char *vp = buf + w * h + uw * uh;
        for (int r = 0; r < uh; r++) {
            memcpy(img.planes[1] + r * img.stride[1], up + r * uw, uw);
            memcpy(img.planes[2] + r * img.stride[2], vp + r * uw, uw);
        }

        int flags = (kf_int > 0 && f % kf_int == 0) ? VPX_EFLAG_FORCE_KF : 0;
        res = vpx_codec_encode(&enc, &img, f, 1, flags, VPX_DL_REALTIME);
        if (res) die(vpx_codec_err_to_string(res));

        const vpx_codec_cx_pkt_t *pkt;
        vpx_codec_iter_t iter = NULL;
        while ((pkt = vpx_codec_get_cx_data(&enc, &iter))) {
            if (pkt->kind != VPX_CODEC_CX_FRAME_PKT) continue;
            if (ivf) ivf_frame(ivf, pkt);
            /* packet dump: len | bytes | flags */
            unsigned int len = pkt->data.frame.sz;
            unsigned int pflags = (pkt->data.frame.flags & VPX_FRAME_IS_KEY) ? 1 : 0;
            fwrite(&len, 4, 1, stdout);
            fwrite(pkt->data.frame.buf, 1, len, stdout);
            fwrite(&pflags, 4, 1, stdout);
            if (pkt->data.frame.flags & VPX_FRAME_IS_INVISIBLE)
                fprintf(stderr, "note: invisible pkt %d\n", f);
        }
    }
    /* flush */
    const vpx_codec_cx_pkt_t *pkt;
    vpx_codec_iter_t iter = NULL;
    do {
        res = vpx_codec_encode(&enc, NULL, -1, 1, 0, VPX_DL_REALTIME);
    } while (0);
    while ((pkt = vpx_codec_get_cx_data(&enc, &iter))) {
        if (pkt->kind == VPX_CODEC_CX_FRAME_PKT) {
            unsigned int len = pkt->data.frame.sz;
            unsigned int pflags = (pkt->data.frame.flags & VPX_FRAME_IS_KEY) ? 1 : 0;
            fwrite(&len, 4, 1, stdout);
            fwrite(pkt->data.frame.buf, 1, len, stdout);
            fwrite(&pflags, 4, 1, stdout);
        }
    }

    vpx_img_free(&img);
    vpx_codec_destroy(&enc);
    free(buf);
    fclose(in);
    if (ivf) fclose(ivf);
    return 0;
}
