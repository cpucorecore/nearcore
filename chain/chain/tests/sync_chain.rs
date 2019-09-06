use near_chain::test_utils::setup;
use near_chain::Block;
use near_primitives::test_utils::init_test_logger;

#[test]
fn chain_sync_headers() {
    init_test_logger();
    let (mut chain, _, signer) = setup();
    assert_eq!(chain.sync_head().unwrap().height, 0);
    let mut blocks = vec![chain.get_block(&chain.genesis().hash()).unwrap().clone()];
    for i in 0..4 {
        blocks.push(Block::empty(&blocks[i], signer.clone()));
    }
    chain.sync_block_headers(blocks.drain(1..).map(|block| block.header).collect()).unwrap();
    assert_eq!(chain.sync_head().unwrap().height, 4);
}