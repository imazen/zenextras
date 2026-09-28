fn main() {
    let f = std::fs::File::open(std::env::args().nth(1).unwrap()).unwrap();
    let src = zencodec_media::mp4::Mp4Demuxer::new(
        std::io::BufReader::new(f),
        zencodec_media::track::MediaLimits::default(),
    )
    .unwrap();
    for t in src.tracks() {
        println!("{:?} codec={:?} tb={}/{} delay_ns={} preroll={} epoch={} edit_delay={:?} decl_pkts={:?} decl_dur={:?}", t.index, t.codec, t.time_base.numerator(), t.time_base.denominator(), t.codec_delay_ns, t.seek_preroll_ns, t.config_epoch, t.edit_delay_ticks, t.declared_packets, t.declared_duration);
    }
}
