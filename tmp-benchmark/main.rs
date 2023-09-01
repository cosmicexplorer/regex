use std::{
    io::Write,
    sync::Arc,
    time::{Duration, Instant},
};

use regex::{Regex, RegexBuilder};

const ITERS: usize = 100_000;
const PATTERN: &str = "";
const HAYSTACK: &str = "ZQZQZQZQ";

#[derive(Debug)]
struct Benchmark {
    re: Regex,
    threads: u32,
}

impl Benchmark {
    fn cloned(&self) -> anyhow::Result<Duration> {
        let start = Instant::now();
        let mut handles = vec![];
        for _ in 0..self.threads {
            // When we clone the regex like this, it does NOT make a complete
            // copy of all of its internal state, but it does create an entirely
            // fresh pool from which to get mutable scratch space for each
            // search. Basically, a 'Regex' internally looks like this:
            //
            //   struct Regex {
            //     // Among other things, this contains the literal
            //     // prefilters and the Thompson VM bytecode
            //     // instructions.
            //     read_only: Arc<ReadOnly>,
            //     // Contains space used by the regex matcher
            //     // during search time. e.g., The DFA transition
            //     // table for the lazy DFA or the set of active
            //     // threads for the Thompson NFA simulation.
            //     pool: Pool<ScratchSpace>,
            //   }
            //
            // That is, a regex already internally uses reference counting,
            // so cloning it does not create an entirely separate copy of the
            // data. It's effectively free. However, cloning it does create
            // an entirely fresh 'Pool'. It specifically does not reuse pools
            // across cloned regexes, and it does this specifically so that
            // callers have a path that permits them to opt out of contention
            // on the pool.
            //
            // Namely, when a fresh pool is created, it activates a special
            // optimization for the first thread that accesses the pool. For
            // that thread gets access to a special value ONLY accessible to
            // that thread, where as all other threads accessing the pool get
            // sent through the "slow" path via a mutex. When a lot of threads
            // share the same regex **with the same pool**, this mutex comes
            // under very heavy contention.
            //
            // It is worth pointing out that the mutex is NOT held for the
            // duration of the search. Effectively what happens is:
            //
            //   is "first" thread optimization active?
            //   NO: mutex lock
            //       pop pointer out of the pool
            //       mutex unlock
            //   do a search
            //   is "first" thread optimization active?
            //   NO: mutex lock
            //       push pointer back into pool
            //       mutex unlock
            //
            // So in the case where "do a search" is extremely fast, i.e., when
            // the haystack is tiny, as in this case, the mutex contention ends
            // up dominating the runtime. As the number of threads increases,
            // the contention gets worse and worse and thus runtime blows up.
            //
            // But, all of that contention can be avoided by giving each thread
            // a fresh regex and thus each one gets its own pool and each
            // thread gets the "first" thread optimization applied. So the
            // internal access for the mutable scratch space now looks like
            // this:
            //
            //   is "first" thread optimization active?
            //   YES: return pointer to special mutable scratch space
            //   do a search
            //   is "first" thread optimization active?
            //   YES: do nothing
            //
            // So how to fix this? Well, it's kind of hard. The regex crate
            // used to use the 'thread_local' crate that optimized this
            // particular access pattern and essentially kept a hash table
            // keyed on thread ID. But this led to other issues. Specifically,
            // its memory usage scaled with the number of active threads using
            // a regex, where as the current approach scales with the number of
            // active threads *simultaneously* using a regex.
            //
            // I am not an expert on concurrent data structures though, so
            // there is likely a better approach. But the idea here is indeed
            // to make it possible to opt out of contention by being able to
            // clone the regex. Once you do that, there are **zero** competing
            // resources between the threads.
            //
            // Why not just do this in all cases? Well, I guess I would if I
            // could, but I don't know how. The reason why explicit cloning
            // permits one to opt out is that each thread is handed its own
            // copy of the regex and its own pool, and that is specifically
            // controlled by the caller. I'm not sure how to do that from
            // within the regex library itself, since it isn't really aware of
            // threads per se.
            let re = self.re.clone();
            handles.push(std::thread::spawn(move || {
                let mut matched = 0;
                for _ in 0..ITERS {
                    if re.is_match(HAYSTACK) {
                        matched += 1;
                    }
                }
                matched
            }));
        }
        let mut matched = 0;
        for h in handles {
            matched += h.join().unwrap();
        }
        assert!(matched > 0);
        Ok(Instant::now().duration_since(start))
    }

    fn shared(&self) -> anyhow::Result<Duration> {
        let start = Instant::now();
        let mut handles = vec![];
        // We clone the regex into an Arc but then share it across all threads.
        // Each thread in turn competes with the single regex's shared memory
        // pool for mutable scratch space to use during a search. This is what
        // ultimately caused this 'shared' benchmark to be much slower than the
        // 'cloned' benchmark when run with many threads. Indeed, profiling it
        // reveals that most of the time is spent in the regex internal 'Pool'
        // type's 'get' and 'get_slow' methods.
        let re = Arc::new(self.re.clone());
        for _ in 0..self.threads {
            let re = Arc::clone(&re);
            handles.push(std::thread::spawn(move || {
                let mut matched = 0;
                for _ in 0..ITERS {
                    if re.is_match(HAYSTACK) {
                        matched += 1;
                    }
                }
                matched
            }));
        }
        let mut matched = 0;
        for h in handles {
            matched += h.join().unwrap();
        }
        assert!(matched > 0);
        Ok(Instant::now().duration_since(start))
    }
}

fn main() -> anyhow::Result<()> {
    let threads: u32 = std::env::var("REGEX_BENCH_THREADS")?.parse()?;
    let re = RegexBuilder::new(PATTERN)
        .unicode(false)
        .dfa_size_limit(50 * (1 << 20))
        .build()?;
    let benchmark = Benchmark { re, threads };
    let which = std::env::var("REGEX_BENCH_WHICH")?;
    let duration = match &*which {
        "cloned" => benchmark.cloned(),
        "shared" => benchmark.shared(),
        unknown => anyhow::bail!("unrecognized REGEX_BENCH_WHICH={}", unknown),
    };
    writeln!(std::io::stdout(), "{:?}", duration)?;
    Ok(())
}
