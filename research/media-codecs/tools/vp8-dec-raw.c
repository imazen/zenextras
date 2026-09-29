/* vp8-dec-raw — minimal raw-libvpx IVF decoder used as the PRIMARY oracle
 * for VP8 streams containing show_frame=0 (invisible) packets.
 *
 * Why it exists: ffmpeg's libvpx wrapper emits a duplicate of the previous
 * shown frame in each suppressed packet's output slot (and ffmpeg's native
 * vp8 decoder shows the hidden frames outright). Only a direct
 * vpx_codec_decode()/vpx_codec_get_frame() loop reproduces libvpx's real
 * emitted sequence.
 *
 * Build (against a configured libvpx tree, e.g. the pinned
 * candidates/libvpx checkout after ./configure && make):
 *   cc -O2 -o vp8-dec-raw vp8-dec-raw.c -I<libvpx-build> -I<libvpx-src> \
 *      <libvpx-build>/libvpx.a -lpthread -lm
 * Or symlink/copy any vpxdec-like binary as tools/vp8-dec-raw, or point
 * VP8_RAW_DEC at one.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "vpx/vpx_decoder.h"
#include "vpx/vp8dx.h"
static uint32_t rd32(const uint8_t*p){return p[0]|p[1]<<8|(uint32_t)p[2]<<16|(uint32_t)p[3]<<24;}
int main(int argc,char**argv){
  FILE*f=fopen(argv[1],"rb");
  uint8_t hdr[32]; fread(hdr,1,32,f);
  FILE*o=fopen(argv[2],"wb");
  vpx_codec_ctx_t ctx;
  vpx_codec_dec_cfg_t cfg={0,0,0};
  vpx_codec_dec_init(&ctx,vpx_codec_vp8_dx(),&cfg,0);
  for(;;){
    uint8_t fh[12];
    if(fread(fh,1,12,f)!=12)break;
    uint32_t sz=rd32(fh);
    uint8_t*pkt=malloc(sz);
    fread(pkt,1,sz,f);
    if(vpx_codec_decode(&ctx,pkt,sz,NULL,0)!=VPX_CODEC_OK){
      fprintf(stderr,"decode err: %s\n",vpx_codec_error(&ctx)); break;
    }
    free(pkt);
    vpx_codec_iter_t it=NULL; vpx_image_t*im;
    while((im=vpx_codec_get_frame(&ctx,&it))){
      int r;
      for(r=0;r<im->d_h;r++)fwrite(im->planes[0]+r*im->stride[0],1,im->d_w,o);
      for(r=0;r<(im->d_h+1)/2;r++)fwrite(im->planes[1]+r*im->stride[1],1,(im->d_w+1)/2,o);
      for(r=0;r<(im->d_h+1)/2;r++)fwrite(im->planes[2]+r*im->stride[2],1,(im->d_w+1)/2,o);
    }
  }
  fclose(o);
  return 0;
}
