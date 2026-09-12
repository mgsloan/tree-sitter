- [ ] Review b3339c87b (AI design polish)

- [ ] Consider what APIs could make it faster

- [ ] specialization for 8-bit symbol or field case? Round 6 or 7 up to 8?

- [ ] Benchmark vs no packing

- [ ] Better to put groups right after their node data! This way page fault etc
  has better locality

- [ ] Swap order of TSPoint for faster lexicographic compare?

- [ ] Tuning:

  * threshold between scan and parent walk

  * symbol presence cache

  * scan window

- [ ] Persistence should do age based deletion

- [ ] API that allows reuse of scratch buffers.  Also share lookup tables derived from grammar
