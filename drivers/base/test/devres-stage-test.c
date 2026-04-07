// SPDX-License-Identifier: GPL-2.0
/*
 * KUnit tests for multi-stage devres functionality
 */

#include <kunit/test.h>
#include <kunit/resource.h>
#include <linux/device.h>
#include <linux/device/faux.h>
#include <linux/device/devres.h>

struct devres_stage_test_priv {
	int release_order[8];
	int release_count;
};

struct devres_test_res {
	struct devres_stage_test_priv *priv;
	int id;
	enum devres_stage stage;
};

/* Release functions - each records its ID in the order array */
static void test_release_reg_1(struct device *dev, void *res)
{
	struct devres_test_res *dr = res;
	dr->priv->release_order[dr->priv->release_count++] = dr->id;
}

static void test_release_reg_2(struct device *dev, void *res)
{
	struct devres_test_res *dr = res;
	dr->priv->release_order[dr->priv->release_count++] = dr->id;
}

static void test_release_default_1(struct device *dev, void *res)
{
	struct devres_test_res *dr = res;
	dr->priv->release_order[dr->priv->release_count++] = dr->id;
}

static void test_release_default_2(struct device *dev, void *res)
{
	struct devres_test_res *dr = res;
	dr->priv->release_order[dr->priv->release_count++] = dr->id;
}

/* Test 1: Basic stage ordering (REGISTRATION → DATA) */
static void devres_stage_order_test(struct kunit *test)
{
	struct faux_device *faux_dev;
	struct device *dev;
	struct devres_stage_test_priv *priv;
	void *res1, *res2, *res3, *res4;

	priv = kunit_kzalloc(test, sizeof(*priv), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, priv);

	faux_dev = faux_device_create("devres-stage-test", NULL, NULL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, faux_dev);
	dev = &faux_dev->dev;

	/* Add DATA stage resources (released second, LIFO) */
	res1 = devres_alloc(test_release_default_1, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res1);
	((struct devres_test_res *)res1)->priv = priv;
	((struct devres_test_res *)res1)->id = 1;
	((struct devres_test_res *)res1)->stage = DEVRES_STAGE_DATA;
	devres_add_stage(dev, res1, DEVRES_STAGE_DATA);

	res2 = devres_alloc(test_release_default_2, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res2);
	((struct devres_test_res *)res2)->priv = priv;
	((struct devres_test_res *)res2)->id = 2;
	((struct devres_test_res *)res2)->stage = DEVRES_STAGE_DATA;
	devres_add_stage(dev, res2, DEVRES_STAGE_DATA);

	/* Add REGISTRATION stage resources (released first, LIFO) */
	res3 = devres_alloc(test_release_reg_1, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res3);
	((struct devres_test_res *)res3)->priv = priv;
	((struct devres_test_res *)res3)->id = 3;
	((struct devres_test_res *)res3)->stage = DEVRES_STAGE_REGISTRATION;
	devres_add_stage(dev, res3, DEVRES_STAGE_REGISTRATION);

	res4 = devres_alloc(test_release_reg_2, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res4);
	((struct devres_test_res *)res4)->priv = priv;
	((struct devres_test_res *)res4)->id = 4;
	((struct devres_test_res *)res4)->stage = DEVRES_STAGE_REGISTRATION;
	devres_add_stage(dev, res4, DEVRES_STAGE_REGISTRATION);

	/* Destroy device - triggers devres_release_all() */
	faux_device_destroy(faux_dev);

	/* Verify release order: REGISTRATION (LIFO) then DATA (LIFO) */
	KUNIT_EXPECT_EQ(test, priv->release_count, 4);
	KUNIT_EXPECT_EQ(test, priv->release_order[0], 4);  /* REG second (last added) */
	KUNIT_EXPECT_EQ(test, priv->release_order[1], 3);  /* REG first */
	KUNIT_EXPECT_EQ(test, priv->release_order[2], 2);  /* DATA second */
	KUNIT_EXPECT_EQ(test, priv->release_order[3], 1);  /* DATA first */
}

/* Test 2: Multi-stage groups */
static void devres_multistage_group_test(struct kunit *test)
{
	struct faux_device *faux_dev;
	struct device *dev;
	struct devres_stage_test_priv *priv;
	void *group_id, *res1, *res2, *res3, *res4;

	priv = kunit_kzalloc(test, sizeof(*priv), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, priv);

	faux_dev = faux_device_create("devres-group-test", NULL, NULL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, faux_dev);
	dev = &faux_dev->dev;

	/* Open multi-stage group */
	group_id = devres_open_group(dev, NULL, GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, group_id);

	/* Add REGISTRATION stage resources to group */
	res1 = devres_alloc(test_release_reg_1, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res1);
	((struct devres_test_res *)res1)->priv = priv;
	((struct devres_test_res *)res1)->id = 3;
	devres_add_stage(dev, res1, DEVRES_STAGE_REGISTRATION);

	res2 = devres_alloc(test_release_reg_2, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res2);
	((struct devres_test_res *)res2)->priv = priv;
	((struct devres_test_res *)res2)->id = 4;
	devres_add_stage(dev, res2, DEVRES_STAGE_REGISTRATION);

	/* Add DATA stage resources to group */
	res3 = devres_alloc(test_release_default_1, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res3);
	((struct devres_test_res *)res3)->priv = priv;
	((struct devres_test_res *)res3)->id = 1;
	devres_add_stage(dev, res3, DEVRES_STAGE_DATA);

	res4 = devres_alloc(test_release_default_2, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res4);
	((struct devres_test_res *)res4)->priv = priv;
	((struct devres_test_res *)res4)->id = 2;
	devres_add_stage(dev, res4, DEVRES_STAGE_DATA);

	/* Close group */
	devres_close_group(dev, group_id);

	/* Release group - should release all stages in order */
	devres_release_group(dev, group_id);

	/* Verify order: REGISTRATION (LIFO) then DATA (LIFO) */
	KUNIT_EXPECT_EQ(test, priv->release_count, 4);
	KUNIT_EXPECT_EQ(test, priv->release_order[0], 4);  /* REG second */
	KUNIT_EXPECT_EQ(test, priv->release_order[1], 3);  /* REG first */
	KUNIT_EXPECT_EQ(test, priv->release_order[2], 2);  /* DATA second */
	KUNIT_EXPECT_EQ(test, priv->release_order[3], 1);  /* DATA first */

	faux_device_destroy(faux_dev);
}

/* Test 3: Bounded group with resources outside */
static void devres_bounded_group_test(struct kunit *test)
{
	struct faux_device *faux_dev;
	struct device *dev;
	struct devres_stage_test_priv *priv;
	void *group_id, *res_outside, *res_in_group;

	priv = kunit_kzalloc(test, sizeof(*priv), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, priv);

	faux_dev = faux_device_create("devres-bounded-test", NULL, NULL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, faux_dev);
	dev = &faux_dev->dev;

	/* Resource OUTSIDE group */
	res_outside = devres_alloc(test_release_default_1, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res_outside);
	((struct devres_test_res *)res_outside)->priv = priv;
	((struct devres_test_res *)res_outside)->id = 100;
	devres_add_stage(dev, res_outside, DEVRES_STAGE_DATA);

	/* Bounded group */
	group_id = devres_open_group(dev, NULL, GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, group_id);

	res_in_group = devres_alloc(test_release_default_2, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res_in_group);
	((struct devres_test_res *)res_in_group)->priv = priv;
	((struct devres_test_res *)res_in_group)->id = 200;
	devres_add_stage(dev, res_in_group, DEVRES_STAGE_DATA);

	devres_close_group(dev, group_id);

	/* Release group - should only release resource IN group */
	devres_release_group(dev, group_id);

	KUNIT_EXPECT_EQ(test, priv->release_count, 1);
	KUNIT_EXPECT_EQ(test, priv->release_order[0], 200);  /* Only group resource */

	/* Destroy device - releases resource OUTSIDE group */
	faux_device_destroy(faux_dev);

	KUNIT_EXPECT_EQ(test, priv->release_count, 2);
	KUNIT_EXPECT_EQ(test, priv->release_order[1], 100);  /* Outside resource */
}

/* Test 4: Backwards compatibility - default stage */
static void devres_backwards_compat_test(struct kunit *test)
{
	struct faux_device *faux_dev;
	struct device *dev;
	struct devres_stage_test_priv *priv;
	void *res;

	priv = kunit_kzalloc(test, sizeof(*priv), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, priv);

	faux_dev = faux_device_create("devres-compat-test", NULL, NULL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, faux_dev);
	dev = &faux_dev->dev;

	/* Use old API without stage - should default to DATA stage */
	res = devres_alloc(test_release_default_1, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res);
	((struct devres_test_res *)res)->priv = priv;
	((struct devres_test_res *)res)->id = 1;
	devres_add(dev, res);  /* Old API - no stage parameter */

	faux_device_destroy(faux_dev);

	KUNIT_EXPECT_EQ(test, priv->release_count, 1);
	KUNIT_EXPECT_EQ(test, priv->release_order[0], 1);
}

/* Test 5: Empty stages - no crash */
static void devres_empty_stage_test(struct kunit *test)
{
	struct faux_device *faux_dev;
	struct device *dev;
	struct devres_stage_test_priv *priv;
	void *res;

	priv = kunit_kzalloc(test, sizeof(*priv), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, priv);

	faux_dev = faux_device_create("devres-empty-test", NULL, NULL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, faux_dev);
	dev = &faux_dev->dev;

	/* Only add to DATA stage, leave REGISTRATION empty */
	res = devres_alloc(test_release_default_1, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res);
	((struct devres_test_res *)res)->priv = priv;
	((struct devres_test_res *)res)->id = 1;
	devres_add_stage(dev, res, DEVRES_STAGE_DATA);

	/* Should not crash with empty REGISTRATION stage */
	faux_device_destroy(faux_dev);

	KUNIT_EXPECT_EQ(test, priv->release_count, 1);
	KUNIT_EXPECT_EQ(test, priv->release_order[0], 1);
}

/* Test 6: Group remove (not release) */
static void devres_group_remove_test(struct kunit *test)
{
	struct faux_device *faux_dev;
	struct device *dev;
	struct devres_stage_test_priv *priv;
	void *group_id, *res;

	priv = kunit_kzalloc(test, sizeof(*priv), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, priv);

	faux_dev = faux_device_create("devres-remove-test", NULL, NULL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, faux_dev);
	dev = &faux_dev->dev;

	group_id = devres_open_group(dev, NULL, GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, group_id);

	res = devres_alloc(test_release_default_1, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, res);
	((struct devres_test_res *)res)->priv = priv;
	((struct devres_test_res *)res)->id = 1;
	devres_add_stage(dev, res, DEVRES_STAGE_DATA);

	devres_close_group(dev, group_id);

	/* Remove group (not release) - resources stay */
	devres_remove_group(dev, group_id);

	KUNIT_EXPECT_EQ(test, priv->release_count, 0);  /* Nothing released yet */

	/* Destroy device - releases ungrouped resources */
	faux_device_destroy(faux_dev);

	KUNIT_EXPECT_EQ(test, priv->release_count, 1);
	KUNIT_EXPECT_EQ(test, priv->release_order[0], 1);
}

/* Test 7: Complex ordering with mixed stages */
static void devres_complex_ordering_test(struct kunit *test)
{
	struct faux_device *faux_dev;
	struct device *dev;
	struct devres_stage_test_priv *priv;
	void *r1, *r2, *r3, *r4, *r5, *r6;

	priv = kunit_kzalloc(test, sizeof(*priv), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, priv);

	faux_dev = faux_device_create("devres-complex-test", NULL, NULL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, faux_dev);
	dev = &faux_dev->dev;

	/* Interleaved additions: DATA, REG, DATA, REG, DATA, REG */
	r1 = devres_alloc(test_release_default_1, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, r1);
	((struct devres_test_res *)r1)->priv = priv;
	((struct devres_test_res *)r1)->id = 10;
	devres_add_stage(dev, r1, DEVRES_STAGE_DATA);

	r2 = devres_alloc(test_release_reg_1, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, r2);
	((struct devres_test_res *)r2)->priv = priv;
	((struct devres_test_res *)r2)->id = 30;
	devres_add_stage(dev, r2, DEVRES_STAGE_REGISTRATION);

	r3 = devres_alloc(test_release_default_2, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, r3);
	((struct devres_test_res *)r3)->priv = priv;
	((struct devres_test_res *)r3)->id = 20;
	devres_add_stage(dev, r3, DEVRES_STAGE_DATA);

	r4 = devres_alloc(test_release_reg_2, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, r4);
	((struct devres_test_res *)r4)->priv = priv;
	((struct devres_test_res *)r4)->id = 40;
	devres_add_stage(dev, r4, DEVRES_STAGE_REGISTRATION);

	r5 = devres_alloc(test_release_default_1, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, r5);
	((struct devres_test_res *)r5)->priv = priv;
	((struct devres_test_res *)r5)->id = 30;
	devres_add_stage(dev, r5, DEVRES_STAGE_DATA);

	r6 = devres_alloc(test_release_reg_1, sizeof(struct devres_test_res), GFP_KERNEL);
	KUNIT_ASSERT_NOT_ERR_OR_NULL(test, r6);
	((struct devres_test_res *)r6)->priv = priv;
	((struct devres_test_res *)r6)->id = 50;
	devres_add_stage(dev, r6, DEVRES_STAGE_REGISTRATION);

	faux_device_destroy(faux_dev);

	/* Verify: REG in LIFO (50, 40, 30), then DATA in LIFO (30, 20, 10) */
	KUNIT_EXPECT_EQ(test, priv->release_count, 6);
	KUNIT_EXPECT_EQ(test, priv->release_order[0], 50);  /* REG last */
	KUNIT_EXPECT_EQ(test, priv->release_order[1], 40);  /* REG middle */
	KUNIT_EXPECT_EQ(test, priv->release_order[2], 30);  /* REG first */
	KUNIT_EXPECT_EQ(test, priv->release_order[3], 30);  /* DATA last */
	KUNIT_EXPECT_EQ(test, priv->release_order[4], 20);  /* DATA middle */
	KUNIT_EXPECT_EQ(test, priv->release_order[5], 10);  /* DATA first */
}

static struct kunit_case devres_stage_test_cases[] = {
	KUNIT_CASE(devres_stage_order_test),
	KUNIT_CASE(devres_multistage_group_test),
	KUNIT_CASE(devres_bounded_group_test),
	KUNIT_CASE(devres_backwards_compat_test),
	KUNIT_CASE(devres_empty_stage_test),
	KUNIT_CASE(devres_group_remove_test),
	KUNIT_CASE(devres_complex_ordering_test),
	{}
};

static struct kunit_suite devres_stage_test_suite = {
	.name = "devres-stage",
	.test_cases = devres_stage_test_cases,
};

kunit_test_suite(devres_stage_test_suite);

MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("KUnit tests for multi-stage devres");
MODULE_AUTHOR("Claude Sonnet 4.5");
